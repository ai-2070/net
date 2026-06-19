//! Retention policy for RedEX (count + size + age).
//!
//! This module is pure logic: given the current index state, the
//! per-entry timestamps, and a config, it returns the number of head
//! entries to evict. The actual segment rewrite happens in
//! `RedexFile::sweep_retention`.

use super::config::RedexFileConfig;
use super::entry::{RedexEntry, REDEX_ENTRY_SIZE};

/// Compute how many head entries to drop so that the retained tail
/// satisfies every active retention policy (`retention_max_events`,
/// `retention_max_bytes`, `retention_max_age_ns`). When multiple are
/// set, the policies AND together — we take the largest drop count
/// so that ALL constraints are satisfied.
///
/// `entries` is the current index in seq order. `timestamps` is a
/// parallel slice of unix-nanos timestamps captured at append time
/// (same length as `entries`). `now_ns` is the current wall clock in
/// unix nanos. Returns a count such that `entries[count..]` is the
/// post-sweep tail.
pub(crate) fn compute_eviction_count(
    entries: &[RedexEntry],
    timestamps: &[u64],
    now_ns: u64,
    cfg: &RedexFileConfig,
) -> usize {
    debug_assert_eq!(
        entries.len(),
        timestamps.len(),
        "timestamps must parallel entries"
    );

    let mut drop = 0usize;

    // Count-based.
    if let Some(max_events) = cfg.retention_max_events {
        let len = entries.len() as u64;
        if len > max_events {
            drop = (len - max_events) as usize;
        }
    }

    // Size-based: walk from the tail back, accumulating bytes; anything
    // beyond the cap is dropped. `idx` here is the forward index — the
    // entry at `entries[idx]`. When it doesn't fit, we drop [0..idx+1).
    //
    // The per-entry size counted against `retention_max_bytes` is the
    // total on-disk / in-memory footprint: 20 bytes of index record
    // plus the heap payload (or 0 for inline, since the payload rides
    // inside the index record). Counting only the payload would make
    // small-payload workloads blow past the cap through index overhead
    // alone.
    if let Some(max_bytes) = cfg.retention_max_bytes {
        let mut retained_bytes: u64 = 0;
        for (idx, e) in entries.iter().enumerate().rev() {
            let size = entry_total_size(e);
            if retained_bytes + size > max_bytes {
                drop = drop.max(idx + 1);
                break;
            }
            retained_bytes += size;
        }
        // If we exited the loop without breaking, all entries fit —
        // size policy drops none.
    }

    // Age-based: drop entries whose timestamp is STRICTLY older
    // than the cutoff. Eviction is head-prefix only (the segment
    // can only trim from the front), so the age drop count is the
    // length of the prefix that must go for no *stale* entry to
    // remain.
    //
    // Pre-fix #36, this early-broke at the first entry with
    // `ts >= cutoff`, treating the prefix before it as "all older"
    // — which assumes timestamps are monotonically non-decreasing.
    // They are wall-clock (`now_ns()`) captured at append time, so
    // a backward clock step (NTP correction) can make a *later*
    // entry carry a *smaller* ts than an earlier one. With the
    // early break, a young entry followed by an older one yields a
    // wrong drop count: the older entry is silently retained past
    // its max age. Fix: scan the whole slice and take the prefix up
    // to and including the LAST entry older than the cutoff. This
    // never retains a stale entry; at worst it drops a young entry
    // that happens to sit ahead of an older one in seq order, which
    // is the conservative choice (over-retention of stale data is
    // the failure we must avoid). On a monotonic clock the last-old
    // index is exactly `first-young-index − 1`, so the count is
    // identical to the old early-break behavior — no change on the
    // happy path.
    //
    // Note on the `ts >= cutoff` boundary: an entry exactly
    // `max_age_ns` old (`ts == cutoff`) is RETAINED, matching the
    // intuitive "max age N retains entries up to N old" semantics.
    if let Some(max_age_ns) = cfg.retention_max_age_ns {
        let cutoff = now_ns.saturating_sub(max_age_ns);
        let mut age_drop = 0;
        for (idx, &ts) in timestamps.iter().enumerate() {
            if ts < cutoff {
                // Drop the prefix [0..=idx]. Keep scanning: a
                // still-later entry may also be stale under a
                // non-monotonic clock.
                age_drop = idx + 1;
            }
        }
        drop = drop.max(age_drop);
    }

    drop
}

/// Total on-disk / in-memory size attributable to one entry: the
/// 20-byte index record plus the heap payload, if any. Inline entries
/// carry their payload inside the 20 bytes and contribute nothing
/// extra.
#[inline]
fn entry_total_size(e: &RedexEntry) -> u64 {
    let idx = REDEX_ENTRY_SIZE as u64;
    if e.is_inline() {
        idx
    } else {
        idx + e.payload_len as u64
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::entry::RedexEntry;
    use super::*;

    fn heap_entries(count: usize, each_size: u32) -> Vec<RedexEntry> {
        (0..count)
            .map(|i| RedexEntry::new_heap(i as u64, (i as u32) * each_size, each_size, 0, 0))
            .collect()
    }

    /// Parallel timestamps — all "now" for count/size tests that
    /// don't exercise the age policy.
    fn dummy_timestamps(count: usize) -> Vec<u64> {
        vec![0; count]
    }

    #[test]
    fn test_no_retention_drops_nothing() {
        let entries = heap_entries(100, 16);
        let ts = dummy_timestamps(100);
        let cfg = RedexFileConfig::default();
        assert_eq!(compute_eviction_count(&entries, &ts, 0, &cfg), 0);
    }

    #[test]
    fn test_count_retention() {
        let entries = heap_entries(100, 16);
        let ts = dummy_timestamps(100);
        let cfg = RedexFileConfig::default().with_retention_max_events(40);
        assert_eq!(compute_eviction_count(&entries, &ts, 0, &cfg), 60);
    }

    #[test]
    fn test_size_retention() {
        // Each heap entry costs 20 (index record) + 16 (payload) = 36
        // bytes against the retention_max_bytes budget. 36 × 13 = 468
        // fits in 480; 36 × 14 = 504 does not → keep 13, drop 87.
        let entries = heap_entries(100, 16);
        let ts = dummy_timestamps(100);
        let cfg = RedexFileConfig::default().with_retention_max_bytes(480);
        assert_eq!(compute_eviction_count(&entries, &ts, 0, &cfg), 87);
    }

    #[test]
    fn test_both_count_and_size_takes_larger_drop() {
        // count policy keeps 40 (drops 60), size policy keeps 13
        // (drops 87) with the same 480-byte budget as
        // test_size_retention. The AND-together rule takes the
        // larger drop count.
        let entries = heap_entries(100, 16);
        let ts = dummy_timestamps(100);
        let cfg = RedexFileConfig::default()
            .with_retention_max_events(40)
            .with_retention_max_bytes(480);
        assert_eq!(compute_eviction_count(&entries, &ts, 0, &cfg), 87);
    }

    #[test]
    fn test_regression_size_retention_counts_index_overhead() {
        // Regression: size retention used to count only the payload
        // bytes, so a workload with tiny payloads and a tight budget
        // could retain far more entries than the configured cap in
        // actual memory/disk usage. Inline entries (payload inside
        // the 20-byte index record) in particular were charged 8
        // bytes each when they actually cost 20.
        //
        // Fix: charge 20 bytes of index record for every entry
        // (inline or heap), plus the heap payload. Inline-only
        // workloads now see the cap respected byte-for-byte.
        use super::super::entry::{payload_checksum, RedexEntry};

        // 10 inline entries. Pre-fix: accounting was 80 bytes; a
        // 100-byte cap retained all 10. Post-fix: accounting is
        // 200 bytes; a 100-byte cap retains at most 5 (100 / 20).
        let entries: Vec<RedexEntry> = (0..10u64)
            .map(|i| {
                let payload = i.to_le_bytes();
                RedexEntry::new_inline(i, &payload, payload_checksum(&payload))
            })
            .collect();
        let ts = dummy_timestamps(10);
        let cfg = RedexFileConfig::default().with_retention_max_bytes(100);
        assert_eq!(
            compute_eviction_count(&entries, &ts, 0, &cfg),
            5,
            "size retention must charge the 20-byte index record per inline entry"
        );
    }

    #[test]
    fn test_empty_index() {
        let cfg = RedexFileConfig::default().with_retention_max_events(10);
        assert_eq!(compute_eviction_count(&[], &[], 0, &cfg), 0);
    }

    // ---- Age policy ----

    fn ts_seq(count: usize, step_ns: u64) -> Vec<u64> {
        (0..count as u64).map(|i| i * step_ns + 1_000).collect()
    }

    #[test]
    fn test_age_retention_drops_older_than_cutoff() {
        // 10 entries, 1 ns apart at t=1000..1009.
        // max_age = 5 ns, now = 1009 → cutoff = 1004. Drop entries
        // with ts < 1004: ts 1000..=1003 = 4 entries.
        //
        // Breaking change: pre-fix, the predicate was
        // `ts > cutoff` (drop on `ts <= cutoff`), so an entry
        // exactly `max_age_ns` old (ts == cutoff) was dropped —
        // 5 entries dropped here. Post-fix uses `ts >= cutoff`
        // (drop on `ts < cutoff`), retaining the boundary entry
        // for intuitive "max age" semantics; 4 entries dropped.
        let entries = heap_entries(10, 16);
        let ts = ts_seq(10, 1);
        let cfg = RedexFileConfig::default().with_retention_max_age(Duration::from_nanos(5));
        assert_eq!(compute_eviction_count(&entries, &ts, 1009, &cfg), 4);
    }

    #[test]
    fn test_age_retention_no_drops_when_all_young() {
        let entries = heap_entries(5, 16);
        // timestamps just below now; max_age large → nothing expires.
        let ts = vec![100, 101, 102, 103, 104];
        let cfg = RedexFileConfig::default().with_retention_max_age(Duration::from_nanos(1000));
        assert_eq!(compute_eviction_count(&entries, &ts, 105, &cfg), 0);
    }

    #[test]
    fn test_age_retention_drops_all_when_all_old() {
        let entries = heap_entries(5, 16);
        let ts = vec![1, 2, 3, 4, 5];
        let cfg = RedexFileConfig::default().with_retention_max_age(Duration::from_nanos(1));
        // cutoff = now - 1 = 99; all ts <= 99 → drop all 5.
        assert_eq!(compute_eviction_count(&entries, &ts, 100, &cfg), 5);
    }

    #[test]
    fn test_age_retention_now_before_any_timestamp_drops_nothing() {
        // Weird edge case: clock skew or tests with small now.
        // All timestamps > now → cutoff = 0 (saturating_sub) → nothing
        // drops.
        let entries = heap_entries(3, 16);
        let ts = vec![1000, 2000, 3000];
        let cfg = RedexFileConfig::default().with_retention_max_age(Duration::from_nanos(100));
        assert_eq!(compute_eviction_count(&entries, &ts, 50, &cfg), 0);
    }

    #[test]
    fn test_combined_count_and_age_takes_larger_drop() {
        // Count says drop 3 (keep newest 2 of 5). Age says drop 4
        // (only newest 1 fits under 2 ns cutoff). Final drop = 4.
        let entries = heap_entries(5, 16);
        let ts = vec![100, 101, 102, 103, 104];
        let cfg = RedexFileConfig::default()
            .with_retention_max_events(2)
            .with_retention_max_age(Duration::from_nanos(2));
        // now=104, cutoff=102 → ts 100,101,102 → 3 old; ts 103 stops.
        // Wait — ts > cutoff is the condition to STOP. So 100,101,102 all <= 102 → 3 drops.
        // Count says drop 3 too. Max is 3.
        assert_eq!(compute_eviction_count(&entries, &ts, 104, &cfg), 3);
    }

    #[test]
    fn test_combined_age_larger_than_count() {
        // 10 entries with small old timestamps. Count says drop 5,
        // age says drop 10. Max = 10 wins.
        let entries = heap_entries(10, 16);
        let ts = vec![1u64; 10];
        let cfg = RedexFileConfig::default()
            .with_retention_max_events(5)
            .with_retention_max_age(Duration::from_nanos(1));
        // now=1000, cutoff=999. All ts = 1 < 999 → drop 10.
        assert_eq!(compute_eviction_count(&entries, &ts, 1000, &cfg), 10);
    }

    /// Regression: an entry with timestamp exactly equal
    /// to the cutoff (`ts == now - max_age_ns`) — i.e. an entry
    /// that is exactly `max_age_ns` old — must be RETAINED, not
    /// dropped. Pre-fix the predicate was `ts > cutoff` (drop on
    /// `ts <= cutoff`), so the boundary entry was dropped. Post-
    /// fix uses `ts >= cutoff` (drop on `ts < cutoff`), aligning
    /// with intuitive "max age N retains entries up to N old"
    /// semantics.
    #[test]
    fn bug23_entry_at_exact_cutoff_is_retained() {
        let entries = heap_entries(3, 16);
        // ts: [10, 15, 20]. now=20, max_age=5 → cutoff=15.
        let ts = vec![10u64, 15, 20];
        let cfg = RedexFileConfig::default().with_retention_max_age(Duration::from_nanos(5));
        // Only ts=10 (< 15) should be dropped; ts=15 (== cutoff)
        // is retained (NEW behavior); ts=20 is fresh.
        assert_eq!(
            compute_eviction_count(&entries, &ts, 20, &cfg),
            1,
            "entry at exactly cutoff (ts=15, max_age=5, now=20) \
             must be retained — pre-fix this dropped 2 entries"
        );
    }

    /// Regression #36: a backward wall-clock step (NTP correction)
    /// can make a LATER entry carry a SMALLER timestamp than an
    /// earlier one. The age scan must not early-break at the first
    /// young entry, or it under-counts and silently retains a stale
    /// entry past its max age.
    #[test]
    fn bug36_non_monotonic_timestamps_count_all_stale() {
        let entries = heap_entries(5, 16);
        // index:        0    1    2    3    4
        // ts (ns):    [100, 500, 200, 110, 120]
        // The clock stepped backward after index 1, so indices 2..4
        // carry smaller timestamps than index 1 despite being
        // appended later.
        let ts = vec![100u64, 500, 200, 110, 120];
        // now = 700, max_age = 300 → cutoff = 400. Entries older
        // than the cutoff (ts < 400): indices 0 (100), 2 (200),
        // 3 (110), 4 (120). Index 1 (500) is young.
        //
        // Early-break (pre-fix) would stop at index 1 → drop 1,
        // silently RETAINING the three stale entries at 2,3,4.
        // Full-scan (post-fix) finds the last stale index is 4, so
        // it drops the whole [0..=4] prefix = 5. Conservative: it
        // also drops the young index-1 entry rather than leave the
        // older tail behind it un-evictable, but no stale entry is
        // ever retained.
        let cfg = RedexFileConfig::default().with_retention_max_age(Duration::from_nanos(300));
        assert_eq!(
            compute_eviction_count(&entries, &ts, 700, &cfg),
            5,
            "non-monotonic timestamps must not let a stale entry \
             survive behind a younger one (pre-fix early-break dropped 1)"
        );
    }

    /// Companion to #36: confirm the full-scan fix is a no-op on a
    /// strictly monotonic clock — the drop count must equal the old
    /// early-break behavior so the happy path is unchanged.
    #[test]
    fn bug36_monotonic_timestamps_unchanged() {
        let entries = heap_entries(6, 16);
        let ts = vec![10u64, 20, 30, 40, 50, 60];
        // now = 60, max_age = 25 → cutoff = 35. ts < 35: 10,20,30
        // (indices 0,1,2). Last stale index 2 → drop 3, identical
        // to the early-break result.
        let cfg = RedexFileConfig::default().with_retention_max_age(Duration::from_nanos(25));
        assert_eq!(compute_eviction_count(&entries, &ts, 60, &cfg), 3);
    }
}
