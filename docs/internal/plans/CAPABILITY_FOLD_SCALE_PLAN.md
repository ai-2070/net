# Capability fold at fleet scale: expiry, memory, write path, cache

## Status

**Slices 0 and 1 accepted; Slices 2–5 implemented, awaiting verification.** Written 2026-10-06 against `LZL0/scaling` at
`806635756`. No slice has landed. The 2026-10-06 design review of revision
`ad4225dee` returned HOLD: the optimization direction stands, but several
selected mechanisms and acceptance gates either changed supported semantics
or could not pass their own proof. Revision `01b18c5d5` applied that review's
required repairs (R1–R9) and its targeted corrections. The re-review of
`01b18c5d5` accepted them and cleared Slice 0 to start, with four
implementation details pinned. This revision records them:

1. Benches that reset a shared fixture run each reset immediately before its
   measured operation (Slice 0).
2. Slice 3's `SmallVec` change applies to the `keys` field of Slice 1's
   per-node record, keeping `rev` (Track C).
3. The expiry wheel removes each chunk from its bucket and from primary
   state in the same critical section (Track A2; needed before Slice 8 can
   be authorized).
4. Slice 2's witness compares against the entry count captured before the
   sweep.

Slices 1–5 proceed in order on Slice 0's numbers. Slices 6–8 (interning,
bitmaps, the expiry wheel) stay held, each pending its own decisions.

### Slice 0 baseline (2026-10-06)

Slice 0 landed in `b76cfb445`. Its review returned HOLD on full acceptance
and asked for three harness repairs, S0-1 to S0-3, plus some reporting
corrections. This baseline was re-measured after those repairs.

- **S0-1:** the mixed workload's expiry is now armed relative to a common
  start.
- **S0-2:** every outcome is asserted.
- **S0-3:** each value below carries one of four labels:
  - **measured:** a timer or the counting allocator read it directly;
  - **counted:** from a `FoldMetrics` or cache counter;
  - **estimated:** computed from a table's capacity;
  - **inferred:** an interpretation, not instrumented.

Rows that only Criterion produced, and rows that only the custom reporter
produced, are named as such.

Measured on Intel i9-14900K, Windows 11, Rust 1.99.0. Clock-speed control
taken around the runs: `routing_table/is_local` 214 ps and
`net_encryption/encrypt/64` 214 ns, matching BENCHMARKS.md's i9 column
(201 ps, 213 ns). Fixture: `sample_capability_set`, **31.4 tags per entry**
(not ~35 as Gap §2 assumed).

Commands:

```bash
cargo bench --features "net fixtures" --bench fold_scale
cargo bench --features "net fixtures" --bench fold_scale_report   # -- footprint cache sweep mixed
```

`fold_scale_report` rejects an unknown section name with exit code 2, so a
typo cannot pass as a green run.

Deviations from Slice 0 as written, all deliberate. The first two were
accepted in review.

- The benches live in two new targets, not in `benches/net.rs`.
  `fold_scale` holds the Criterion timings. `fold_scale_report` is a custom
  main for the counted workloads. Building two 1M-entry folds would add
  minutes and several GB to every `net` bench run.
- The footprint probe is part of `fold_scale_report`, not a `#[ignore]`
  test in `fold/tests.rs`. Its counting allocator is switched on **only**
  while the footprint section runs; every timed section in the reporter
  runs with counting off.
- **Lock-hold tails are not measured.** The reporter gives per-operation
  service latency and the duration of whole `sweep_expired_now` calls. A
  sweep call spans many write-locked chunks, so it is not one hold.
  Measuring chunk holds directly is a prerequisite for choosing any
  hold-duration guarantee or retuning `SWEEP_CHUNK_SIZE`. It is not part
  of this slice.

Library changes, instrumentation only:

- `FoldMetrics::{sweep_walks, sweep_yielded}`: per-walk counters, not
  added to the serialized `FoldStats`.
- `CapabilitySetCache::stats()`: hits, stale misses and absent misses.
- A corrected `SWEEP_CHUNK_SIZE` doc comment. It bounds entries per lock
  acquisition, not hold duration.

The two counters each have a unit test.

**Apply and translate** (Criterion only; measured). Each iteration gets a
fresh owned envelope under `BatchSize::PerIteration`, and its outcome is
asserted. The 1,024 apply targets are sampled uniformly at random with a
fixed seed. The first run's fixed stride aliased with the 4,800-entry
template period at 1M, where no target carried `inference`. The sampled
targets carry `inference` at 51.1% / 50.6% / 50.7% (10k / 100k / 1M),
against 50% across the fleet. These are timings of a warm, randomly
sampled target subset.

| operation | 10k | 100k | 1M |
|---|---|---|---|
| `capability_fold_apply/insert` | 5.94 µs | 6.29 µs | 5.99 µs |
| `capability_fold_apply/refresh_equivalent` | 1.27 µs | 1.35 µs | 1.40 µs |
| `capability_fold_apply/replace_changed` | 12.9 µs | 14.8 µs | 16.4 µs |
| `capability_translate/announcement` | 9.79 µs (first run; inputs unchanged) | | |

**Sweep.**

- Steady and 10% rows exist in both targets. The Criterion times
  (measured) are 0.18 ms / 92 ms at 100k and 5.47 ms / 1.43 s at 1M.
- Whole-fleet and evict-only rows exist **only** in the reporter.
- The table is the reporter's run with counting off. Walks and yielded are
  counted; times are measured.
- The 1M 10% sweep measured 1.43 s in Criterion and 1.44 s and 1.83 s in
  two reporter runs. Treat it as 1.4–1.8 s, run to run.

| entries | case | walks | yielded | sweep | evict same set (diagnostic) |
|---|---|---|---|---|---|
| 100k | steady | 1 | 100,000 | 0.49 ms | – |
| 100k | 10% expired | 11 | 608,455 | 92 ms | 87 ms |
| 100k | whole fleet | 99 | 100,000 | 959 ms | 854 ms |
| 1M | steady | 1 | 1,000,000 | 5.66 ms | – |
| 1M | 10% expired | 99 | 45,736,877 | 1.83 s | 1.10 s |
| 1M | whole fleet | 978 | 1,000,000 | 14.1 s | 11.7 s |

The evict column removes the same entries through `evict_node`. That is
one per-node lock acquisition at a time, in node order, after the sweep's
removal and re-insertion have changed the table's history. **The gap
between the two columns is a diagnostic comparison, not a measurement of
walk cost and not a lower bound.** No gate depends on it.

**Footprint** (bytes per entry). Retained, payload heap and cumulative are
measured by the counting allocator. Primary and reverse are estimated from
table capacity. Index is the remainder, so it absorbs any estimation error.

| configuration | entries | retained | payload heap | primary (est.) | reverse (est.) | index (remainder) | cumulative |
|---|---|---|---|---|---|---|---|
| fixture repetition | 100k | 3,293 | 1,641 | 662 | 159 | 832 | 6,582 |
| after 3× 20% churn | 100k | 4,048 | 1,641 | 1,324 | 233 | 849 | 10,606 |
| +7 unique tags per node | 100k | 5,441 | 2,506 | 662 | 159 | 2,114 | 10,178 |
| fixture repetition | **1M** | **4,184** | 1,641 | 1,059 | 204 | 1,280 | 8,363 |

At 1M the fold retains **4.2 GB** (measured). The first baseline's
"3.3 GB at 1M" was a linear extrapolation from 100k. It missed capacity
steps: the primary table rises from 662 to 1,059 B/entry and the index
remainder from 832 to 1,280. It is withdrawn.

**Cache** (reporter; counts). The fold has 10k publishers. The cache has
its default capacity of 256. Lookups run at 100k/s, uniform over the hot
set, for 5 s. Load applies are asserted accepted Replaces. The achieved
rate is reported, not just the requested one.

The fixture keeps the operating point's fleet-wide rate (6,667/s) over 10k
publishers. Each publisher therefore refreshes ~100× more often than in a
real 1M fleet. That does not matter for today's global-generation cache.
It will once validity is per publisher, so re-measure after Slice 1.

| hot set | target apply rate | achieved | hit rate | coherence misses | capacity misses |
|---|---|---|---|---|---|
| 200 | 0 | – | 100.0% | 0 | 0 |
| 200 | 6,667/s | 6,664/s | 33.8% | 330,565 | 0 |
| 1,000 | 0 | – | 25.7% | 0 | 370,590 |
| 1,000 | 6,667/s | 6,664/s | 7.2% | 92,069 | 370,470 |

**Mixed** (reporter; measured; counting off). The workload runs at 1M
resident for 20 s, with all workers released together by a barrier:

- refreshes at 6,667/s, each asserted an accepted Replace;
- a selective query (100 matches, each result size asserted) at 1,000/s;
- a broad query at 2/s, each result checked against the population: no id
  outside the pre-expiry set, and every post-expiry carrier present;
- the 500 ms sweeper.

The 10k-entry batch was built live with the rest of the fold. It was armed
in the first 41 ms after the common start with a 10 s TTL, so every
deadline falls in [10.00 s, 10.04 s). The run asserts:

- every tick that reaped ended at or after the 10 s boundary;
- the sweeps reaped exactly the batch, and no batch node remains;
- final residency is 1M − 10k.

The expiry event was **one tick at 10.15 s that reaped all 10,000 entries
in 160 ms.**

Latency is per-call service time from each op's start, with no
coordinated-omission correction. Streams with under 1,000 samples report
only p50 and max.

| stream | achieved | samples | p50 | p99 | p99.9 | max |
|---|---|---|---|---|---|---|
| refresh apply | 6,666/s | 133,325 | 21.7 µs | 52.6 µs | 202 µs | 37.1 ms |
| selective query | 1,000/s | 19,999 | 6.7 µs | 253 µs | 33.5 ms | 36.2 ms |
| broad query (495k–500k matches) | 1.9/s | 39 | 40.5 ms | – | – | 43.4 ms |
| sweep tick | 2.0/s | 40 | 5.8 ms | – | – | 160 ms |

The run before the S0-1 repair armed the batch during fixture
construction. Its expiry was not a 10 s in-run event, and that row is
withdrawn.

### Slice 0 closure

The re-review closed S0-1 to S0-3 at `5f5695b72`.

- The repository gate was held on two exact-head CI failures:
  - **WebRTC:** a 180 s hang in
    `rtc_repairs::a_frame_captured_under_a_retired_incarnation_cannot_revive_its_reassembly`.
  - **@net-mesh/browser:** a `lobby.test.ts` failure, "no local node … on
    this mesh".
- Both passed on attempt 2 of run 37427000792 at the same SHA: 225/225
  and 964/964.
- Neither is attributable to the range. `b76cfb445..5f5695b72` changes one
  doc comment under `src/`, and the browser job builds only `leaf`, which
  the branch does not touch.
- Neither reproduces locally:
  - the WebRTC test passed 30/30 alone, and the whole `rtc_repairs` binary
    5/5;
  - the lobby test passed 500/500, plus 8 pinned-id variants.
- C consumers (windows-latest) hits its 75-minute limit on `master` itself:
  5 of the last 6 runs, including the base commit `806635756`.

Root causes of the two flakes are not established. They, and the Windows
timeout, are handed off as separate issues.

### Slice 1 (publisher-revision cache validity)

Implemented as Track D specifies.

- **The per-node revision.** `FoldState::by_node` values are now
  `NodeRecord { keys, rev }`. `rev` comes from one fold-wide counter
  (`FoldState::last_rev`) that is never reset, including by
  `clear_for_restore`.
- **One choke point.** Every write to the reverse index goes through
  `attach_key`, `detach_key` or `remove_node` on `FoldState`, the only
  code that moves a revision. That covers insert, replace (both halves),
  `evict_node`, expiry and restore.
- **Absent publishers.** A publisher with no record reads as revision `0`
  through `publisher_rev`. A returning or restored publisher gets a
  revision it never had before.
- **The cache.** `CapabilitySetCache` keys entries on
  `(node_id, publisher_rev)`. `get_or_synthesize` reads the revision,
  checks validity, and on a miss synthesizes and stores, all inside one
  fold read borrow.
- **Owner-projection retraction** (`with_state_mut`) does not advance the
  revision, because the cached set reads only tags and metadata. That
  assumption is documented on the cache.
- **Readers.** In-crate readers use `FoldState::keys_for(node)` instead
  of `by_node.get(&node)`.

**Source-compatibility note.** `FoldState::by_node` is a public field, and
its value type changed from `HashSet<K::Key>` to `NodeRecord<K::Key>`.
In-tree consumers outside the crate (the Go, Node and Python placement
bindings) use only `by_node.contains_key`, which is unaffected.
`cargo check --workspace --all-targets` is clean.

**Trade-off.** A cache hit now takes one fold read lock as well as the
cache mutex. It used to read the change generation through a
`tokio::sync::watch` borrow, without the fold-state lock. Hits can now
queue behind a fold writer (fair `RwLock`), even when the lookup itself
needs no synthesis. The review accepted the trade on the measured
comparison below. The old miss rate alone would not have justified it.

**Proof** (`fold_scale_report -- cache`, same machine and fixture as
Slice 0). The workload adds an "outside hot set" configuration in which
only publishers outside the hot set announce, and **asserts zero stale
misses** there.

| hot set | announcing | apply rate | hit rate (Slice 0 → Slice 1) | coherence misses | capacity misses |
|---|---|---|---|---|---|
| 200 | none | 0 | 100.0% → 100.0% | 0 | 0 |
| 200 | all | 6,667/s | 33.8% → **99.8%** | 330,565 → 651 | 0 |
| 200 | outside hot set | 6,667/s | – → **100.0%** | – → **0** | 0 |
| 1,000 | none | 0 | 25.7% → 25.7% | 0 | 370,590 → 370,611 |
| 1,000 | all | 6,667/s | 7.2% → 25.6% | 92,069 → 116 | 370,470 → 370,636 |
| 1,000 | outside hot set | 6,667/s | – → 25.7% | – → **0** | 370,514 |

The 651 remaining misses at hot set 200 with everyone announcing are the
hot set's own refreshes: 2% of 6,667/s × 5 s ≈ 667, as validity requires.
Capacity misses are unchanged, as Slice 1 intends; capacity sizing remains
the separate follow-up. As the Slice 0 review noted, this fixture gives
each publisher ~100× a real 1M fleet's refresh rate, so own-publisher
misses here are an upper bound.

Apply cost is unchanged within noise: `capability_fold_apply` at 100k is
6.17 µs insert, 1.38 µs refresh and 15.3 µs replace_changed, against
6.29 / 1.35 / 14.8 µs before.

**Release migration note (source-breaking).** The review classified the
`by_node` change as a Rust source-breaking change for the intended
breaking release, not a compatibility-preserving patch. This note
carries into that release's notes:

- `FoldState::by_node` values are `NodeRecord { keys, rev }`, no longer
  `HashSet<K::Key>`. Code such as
  `s.by_node.get(&node).map_or(0, |keys| keys.len())` fails with E0599.
  Migrate to `s.keys_for(node)`, which returns `Option<&HashSet<K::Key>>`.
  `by_node.contains_key` and `by_node.keys()` are unchanged.
- `FoldState` has a new private field (`last_rev`), so external struct
  literals no longer compile. Construct it with `FoldState::new()` or
  `Default`.
- New public API: `NodeRecord`, `FoldState::keys_for`,
  `FoldState::publisher_rev`.

Workspace compilation proves that the known consumers build: the Go,
Node and Python placement bindings use only `contains_key`, and the
`fold_scale_report` bench was migrated to `record.keys`. It does not
prove that unknown downstream crates are unaffected. A parallel revision
map that would preserve the old value type was rejected, because it
would add another fleet-sized structure for a hypothetical reader.

**Cache-hit latency (review-measured comparison).** The review linked the
same fold library against both cache policies: the old global-generation
cache from `5f5695b7` and the publisher-revision cache. This isolates the
policy on one fold; it is not an old-release versus new-release benchmark.

Conditions:

- a warmed 200-node cache, with 100k lookups/s requested;
- outside-hot-set mutations at ~6,667/s;
- 3 s per case, with no counting allocator;
- per-call service latency, with no coordinated-omission correction;
- the large case at 1M resident, with broad queries at 2/s.

| workload | old cache p50 / p99 | publisher cache p50 / p99 |
|---|---|---|
| quiet, 10k resident | 0.100 / 0.500 µs | 0.100 / 0.600 µs |
| outside updates, 10k resident | 7.403 / 48.223 µs | 0.100 / 1.300 µs |
| outside updates + broad reads, 1M resident | 7.003 / 64.031 µs | 0.100 / 1.100 µs |

Worst-case coupling remains. In the short 1M sample the maxima were
~56.6 ms (old) and ~71.6 ms (publisher cache). These are diagnostic, not
evidence of a tail regression and not an SLO bound. Both policies still
expose waits of tens of milliseconds behind the fold lock, and the new
path can wait even on a hit. This comparison is kept through the expiry
and query slices. 99.8% hits does not mean uniformly nanosecond-scale
admission.

**Witnesses** (`fold/capability_bridge.rs` tests; 216 fold tests pass):

- `cache_survives_other_publishers_announcements`
- `cache_misses_on_sibling_class_change`
- `cache_misses_on_older_sibling_expiry`
- `cache_unknown_to_known`
- `cache_misses_after_eviction_and_reannouncement`
- `cache_misses_after_forced_restore`
- `cache_concurrent_miss_populate_never_serves_stale`: 3 readers against
  2,000 writer applies. Every lookup must see at least the version
  applied before it started.

`capability_set_cache_stats_split_absent_and_stale_misses` now produces
its stale miss from the publisher's own replace. Under the old cache it
came from another publisher's apply.

Inverse checks, each caught by its witness, with the code restored
afterwards:

- Not advancing the revision on a partial class removal fails
  `cache_misses_on_older_sibling_expiry`.
- Resetting the revision counter on restore fails
  `cache_misses_after_forced_restore`.

Also green:

- the full library unit suite, `cargo tl`: 5,985 passed;
- 23 capability-related integration binaries: 282 passed;
- fmt, all-targets/all-features clippy, strict lib clippy, rustdoc, and
  `cargo check --workspace --all-targets`.

### Slice 1 acceptance

Accepted at `be8559246`. Exact-head CI run 37443056262 is green: 64
successes and 2 skipped optional checks.

Documentation corrections the review asked for, applied with Slice 2:

- **The old generation read.** Its description is corrected: it was a
  `watch` borrow without the fold-state lock, not a lock-free atomic.
- **`publisher_rev`'s history statement** now applies only to nonzero
  values. Two reads of `0` can bracket the publisher arriving and leaving
  again.
- **Obsolete wording removed.** `Fold::change_generation` no longer
  describes the generation-keyed cache, and the cache's "out-of-order
  store" narrative is replaced. Different revisions cannot cross an
  intervening writer while synthesis and store share one state borrow.

The release migration note and the review's cache-latency comparison are
recorded under Slice 1 above.

### Slice 2 (single-pass expiry sweep)

Implemented as Track A1 specifies. `sweep_expired` is now two phases:

- **`collect_expired`:** ONE walk of `entries` under the state read lock,
  collecting every expired key.
- **`evict_chunk`:** the collected keys in `SWEEP_CHUNK_SIZE` slices, each
  under its own state + index write-lock acquisition. The per-key
  `expires_at <= now` re-check is unchanged.

The two phases are separate functions so a test can refresh or evict an
entry between them. Every eviction still goes through
`FoldState::detach_key`, so Slice 1's revisions advance on expiry exactly
as before. Each sweep records one walk; `sweep_yielded` counts the walk's
entries.

**Proof.** Walks and yielded are counted; times are measured, the
Criterion ones in `fold_scale`, the rest in the reporter. Same machine and
fixture as Slice 0.

| entries | case | walks (S0 → S2) | yielded (S0 → S2) | sweep (S0 → S2) |
|---|---|---|---|---|
| 100k | steady | 1 → 1 | 100,000 → 100,000 | 0.18 → 0.18 ms (Criterion) |
| 100k | 10% expired | 11 → **1** | 608,455 → **100,000** | 92 → 89 ms (Criterion) |
| 100k | whole fleet | 99 → **1** | 100,000 → 100,000 | 959 → 918 ms (reporter) |
| 1M | steady | 1 → 1 | 1,000,000 → 1,000,000 | 5.47 → 5.31 ms (Criterion) |
| 1M | 10% expired | 99 → **1** | 45,736,877 → **1,000,000** | 1.43 → **1.18 s** (Criterion) |
| 1M | whole fleet | 978 → **1** | 1,000,000 → 1,000,000 | 14.1 → 13.9 s (reporter) |

As Slice 0 predicted, removal dominates the remaining time. The 1M 10%
sweep (1.18 s) is close to evicting the same set through `evict_node`
(1.09 s, diagnostic), so Track C's removal path is where the rest of the
time lives. Nothing here is a 10× wall-time claim.

**Reader and writer tails** (mixed workload, 1M resident, measured, same
conditions as the Slice 0 table):

| stream | Slice 0 p50 / p99 / p99.9 / max | Slice 2 p50 / p99 / p99.9 / max |
|---|---|---|
| refresh apply | 21.7 µs / 52.6 µs / 202 µs / 37.1 ms | 21.6 µs / 53.0 µs / 187 µs / 37.9 ms |
| selective query | 6.7 µs / 253 µs / 33.5 ms / 36.2 ms | 7.0 µs / 251 µs / 34.2 ms / 37.1 ms |
| broad query (39 samples) | 40.5 ms p50 / 43.4 ms max | 41.2 ms p50 / 43.9 ms max |
| expiry tick (10k reaped at 10.15 s) | 160 ms | 134 ms |

The tails are unchanged within one run's variation. That is consistent
with Slice 0's finding (inferred) that the ~37 ms maxima come from the
broad query's ~40 ms read hold, not from the sweep. The single
collection walk now holds the read lock for one full pass. At 1M that is
the 5.3 ms `steady` sweep, and it did not show up as a new tail at this
load. **Chunk holds are still not measured directly.** No hold-duration
guarantee or `SWEEP_CHUNK_SIZE` retuning is made, per the Slice 0 review.

**Witnesses** (`fold/tests.rs`; 218 fold tests pass):

- `mass_expiry_yields_each_entry_once`: 20k entries, 2k expired (two
  eviction chunks). Asserts one walk, yielded ≤ the entry count captured
  before the sweep, exact eviction count and final residency.
- `sweep_rechecks_entries_refreshed_or_removed_after_collection`: collect,
  then refresh one entry and `evict_node` another, then evict. Only the
  untouched entry goes, and the metrics count one expiry.
- `sweep_metrics_count_walks_and_yielded_entries`: updated from Slice 0's
  "one walk per chunk plus a final walk" to one walk per sweep, every
  entry yielded exactly once.

The three existing sweep tests (`fold/tests.rs`
`sweep_expired_removes_entries_past_ttl`,
`sweep_with_no_expired_entries_is_a_no_op`,
`sweep_evicts_across_multiple_chunks_when_count_exceeds_chunk_size`) and
`background_sweeper_evicts_expired_entries_on_tick` pass. So do all of
Slice 1's cache witnesses, including `cache_misses_on_older_sibling_expiry`,
which exercises expiry through the new path.

Inverse checks, each caught by its witness, with the code restored
afterwards:

- Replacing the re-check with a bare presence check fails
  `sweep_rechecks_entries_refreshed_or_removed_after_collection`.
- Restoring the restart-per-chunk walk fails
  `mass_expiry_yields_each_entry_once`.

Also green: the full library unit suite (`cargo tl`, 5,987 passed), fmt,
all-targets/all-features clippy, strict lib clippy, and rustdoc.

**S2-1 repair (review HOLD at `bd9716426`).** `collect_expired` recorded
`state.entries.len()` as the walk's yield count. That is the walk's
expected cost, not its observed one. A walk that visited live entries
twice still reported the right number, and every witness stayed green.
The review demonstrated this with an output-preserving extra traversal.

The repair restores a local counter before the expiry filter and
aggregates it into `FoldMetrics` once per walk, so there are still no
per-entry atomics. With the review's inverse applied, an extra
`.chain(...)` pass over the live entries placed before the filter, two
witnesses now fail:

- `sweep_metrics_count_walks_and_yielded_entries`: 200 yielded against
  100 expected;
- `mass_expiry_yields_each_entry_once`: "yielded 38000 entries, more than
  the 20000 present before the sweep".

Restored, all three sweep witnesses and all 218 fold tests pass. The
runtime factoring and every measurement above are unchanged; only the
counter's source moved.

### Slices 3–5: review cadence

By the owner's decision, Slices 3–5 were implemented back to back,
without waiting for exact-head CI or a per-slice review between them.
They are verified together before merging. Slice 2's S2-1 repair is in
`f6b90e500`. Its exact-head CI passed every job except C consumers
(windows-latest), which timed out at 75 minutes, the problem since fixed
on `master` (`7837be561`). `master` has not been merged into this branch.

### Slice 3 (allocation-free apply; removal path)

Implemented as Track C specifies, with the removal-path extension the
Slice 0 review accepted.

- **Grammar-exact borrowing tag reader.** `tag::axis_value_ref(s)`
  returns `Some((axis, key, value))` exactly when `Tag::parse(s)` returns
  `AxisValue`, without allocating. The separator rule (the first of `=`
  and `:`) is factored into one `axis_separator` helper, which
  `parse_axis_body` and the new reader share, so the two cannot disagree.
- **Synthetic tags without allocation.** `for_each_synthetic_index_tag`
  replaces the `Tag::parse`-based derivation on both insert and remove.
  It builds each `model:` / `tool:` / `gpu:` key in a scratch `String`
  that the index owns and reuses.
- **Get-first index inserts.** `on_insert` probes the bucket with the
  borrowed tag and allocates the owned `String` key only for a new
  bucket. `on_remove` mirrors this through the same helpers.
- **In-place replace.** `Fold::apply`'s Replace arm overwrites the entry
  in place. A same-owner replace leaves the reverse-index membership alone
  and calls `FoldState::touch_node`, so the revision still advances, as
  pinned. A different-owner replace (folds keyed on payload alone) moves
  the key between records.
- **Reverse-index keys.** `NodeRecord::keys` is now
  `SmallVec<[K::Key; 1]>`. The record and its `rev` are kept, as pinned.
  `smallvec` becomes a direct dependency at the 1.16.2 already in
  `Cargo.lock`. `FoldState::keys_for` returns `Option<&[K::Key]>`, and the
  Slice 1 migration note's `keys_for` example still compiles.
- **Single-pass translate.** `translate_announcement` renders tags into
  pre-sized buffers, finds the region, and collects the hardware-axis
  tags in one pass. It then decodes the hardware projection from that
  sorted subset rather than from all tags sorted.
  `single_pass_translate_matches_views_projection` pins the equivalence.

**Allocation gate** (`fold_scale_report -- alloc`, counted). Allocation
calls (alloc + realloc) are counted only around the measured calls, and
every envelope is built before counting starts.

| operation (1,000 calls) | allocations | per call |
|---|---|---|
| refresh, index-equivalent, warm (**gated at 0**) | **0** | 0.00 |
| replace, index changed | 11 | 0.01 |
| evict (removal path) | 0 | 0.00 |
| insert (cold) | 70 | 0.07 |
| `translate_announcement` | 63,369 | 63.4 (131.8 before pre-sizing) |

The gate is live. Planting one `format!` in the same-owner replace arm
makes the section count 1,000 allocations and panic. Restored, it reads
0. Removal frees the entry's allocations by definition, and no
"no deallocation" promise is made.

**Timings** (Criterion; measured; same machine and fixture):

| operation | 10k | 100k | 1M |
|---|---|---|---|
| `capability_fold_apply/insert` | 5.94 → **2.91 µs** | 6.29 → **2.84 µs** | 5.99 → **2.89 µs** |
| `capability_fold_apply/refresh_equivalent` | 1.27 → **1.17 µs** | 1.35 → **1.21 µs** | 1.40 → **1.23 µs** |
| `capability_fold_apply/replace_changed` | 12.9 → **7.4 µs** | 14.8 → **8.7 µs** | 16.4 → **10.5 µs** |
| `capability_translate/announcement` | 9.79 → **3.68 µs** | | |

Sweep: Criterion for steady and 10%, the reporter for whole fleet and
evict-only.

| entries | case | Slice 2 | Slice 3 | evict same set (diagnostic) |
|---|---|---|---|---|
| 100k | 10% expired | 89 ms | **66 ms** | 87 → 65 ms |
| 100k | whole fleet | 918 ms | **692 ms** | 878 → 638 ms |
| 1M | 10% expired | 1.18 s | **0.91 s** | 1.09 → 0.88 s |
| 1M | whole fleet | 13.9 s | **11.4 s** | 11.7 → 9.6 s |

Removal is now ~8.8 µs per entry at 1M (diagnostic), down from ~11. The
rest is inherent to removal:

- dropping the payload (~31 `String`s, the `Vec`, the metadata map);
- ~31 `by_tag` lookups, SipHashed on publisher-chosen strings (kept by
  design, per `CapabilityIndexInner`'s doc).

**Mixed workload** (1M resident; same conditions as Slices 0 and 2):

- Refresh apply p50 / p99 / p99.9 went from 21.6 / 53.0 / 187 µs to
  **15.6 / 41.3 / 133 µs**.
- Selective query p99 went from 251 to **190 µs**.
- The expiry tick that reaped the 10k batch took 101.8 ms, down from
  134.
- Maxima are unchanged at ~37 ms (apply, selective query) and ~44 ms
  (broad query). They are still set by the broad query's read hold
  (inferred).

**Witnesses:**

- `synthetic_derivation_matches_tag_parse` (`fold/capability.rs`): the
  differential test against the old `Tag::parse` derivation, kept as a
  `#[cfg(test)]` oracle. It covers both separators, embedded delimiters,
  empty values, bundle-key and index shapes, reserved prefixes, raw tags
  that look like synthetic names, duplicates, and four hardware shapes.
- `axis_value_ref_agrees_with_parse` (`tag.rs`): 23 strings, including
  empty strings, bare `=`, reserved prefixes, unknown axes, and
  `software.` with an empty body.
- `same_key_replace_advances_rev_without_touching_membership`: pinned
  in review. An index-equivalent and an index-changing replace both keep
  `keys` unchanged and advance the revision, and the cached set misses
  exactly once.
- `single_pass_translate_matches_views_projection`: hardware summary,
  tag set and region match the full `views()` path over five sets,
  including a three-GPU mixed-vendor set.
- `target_matches_filter_agrees_with_find_nodes_matching`,
  `find_nodes_matching_dedupes_publisher_across_classes`, and every
  Slice 1 cache witness and Slice 2 sweep witness pass.

Also green:

- fold and tag tests: 239;
- the full library unit suite, `cargo tl`: 5,991 passed;
- 23 capability-related integration binaries: 282 passed;
- fmt; clippy across all targets and features; strict lib clippy at
  all, default and no-default features;
- rustdoc;
- `cargo check --workspace --all-targets`.

### What Slice 0 changes about the later slices

The Slice 0 review acknowledged items 1 and 2 and set their bounds.

1. **Removal work is substantial; Slice 2 is a scan repair, not a
   wall-time 10×.**
   - Removing an entry through `evict_node` takes ~9 µs at 100k and
     ~12 µs at 1M (measured diagnostic), against a 6 µs insert.
   - The yielded-entry count matches Gap §1's corrected estimate: 45.7M
     counted, against (N − E)·E/2048 ≈ 44M.
   - **Slice 2's normative gate is unchanged** and already counts visits:
     one candidate walk, yielded entries at most the captured pre-sweep
     cardinality, exact eviction, and the refreshed-entry re-check. The
     first baseline's reference to a "10× wall-time" gate pointed at
     the retired first draft and is withdrawn.
   - Slice 2 measures whole-sweep duration and reader/writer tails
     separately. It uses the evict-only arm as a diagnostic only.
   - `SWEEP_CHUNK_SIZE`'s doc now says it bounds entries per hold, with a
     workload-dependent duration. Chunk holds must be measured directly
     before any hold guarantee or retuning.
   - **Track C extends to the removal path** (accepted): see Track C.
2. **The cache has two problems; Slice 1 fixes coherence only**
   (accepted).
   - Fleet-wide announcements cut a 200-node hot set from 100% to 34%
     hits.
   - A hot set larger than the 256-entry capacity hits 26% with no
     announcements at all.
   - Slice 1 stays focused on publisher revisions and its
     zero-other-publisher-coherence-miss witness. It leaves the capacity
     at 256 and does not auto-grow the cache with fleet size.
   - Operator capacity configuration is a separate, bounded follow-up,
     sized against observed hot sets and retained synthesized-set bytes.
     `CapabilitySetCache::with_capacity` exists, but `MeshNode` builds the
     cache with `CapabilitySetCache::new()` (`mesh.rs:14125`). Operators
     cannot configure it today, and that follow-up must add the plumbing.
3. **Fold CPU is not the bottleneck at the operating point** (inferred
   from measured per-op times). Translate plus refresh is ~11 µs per
   announcement, about 7% of one core at 6,667/s. Translate (9.8 µs) costs
   more than the apply it feeds.
4. **Memory is 4.2 GB at 1M** (measured).
   - Payload heap is ~40% of it, the index remainder ~31%, and the primary
     table ~25% (`FoldEntry` is 488 B inline).
   - At 100k, churn grows retained bytes 23%, mostly from the primary
     table doubling its capacity (estimated).
   - Seven unique tags per node add 2.1 KB at 100k, mostly index. Interning
     would not help unique tags, which supports R7's separate payload and
     index targets.
5. **The worst tails sit at the broad query's read hold** (inferred, not
   instrumented). Apply and selective-query maxima (~36–37 ms) are just
   under the broad query's ~40 ms service time. The suspected mechanism is
   a writer queued behind the broad read, and readers queued behind that
   writer (fair `RwLock`). Lock-hold instrumentation would confirm it; see
   the deviation above.

What the repairs changed, in short:

- The cache validity token is a **publisher-level revision**, not a
  per-entry stamp (R2).
- Bitmaps key on **(class, node) entries**, and results keep their explicit
  `NodeId` sort (R1).
- Interning and bitmaps are **held** until their representation, memory
  budget, class semantics and ordering are written down (R1, R4, R7).
- The expiry wheel, still conditional, has an explicit deadline-rounding and
  one-placement-per-key contract (R3).
- Slice 0's benchmarks prepare and reset state outside timing and assert
  every outcome (R6). Slice 3 owns the `by_node` refresh allocation (R5).
- The hasher seam uses a required associated type (R8). The borrowing
  synthetic-tag derivation keeps the full tag grammar (R9).
- The cheap query fixes move ahead of the representation rewrite.

Successor to [`CAPABILITY_QUERY_FAST_PATH_PLAN.md`](CAPABILITY_QUERY_FAST_PATH_PLAN.md),
which made the bulk query path O(matches). This plan is about everything that
plan deliberately left alone: the expiry sweep, the per-entry memory
footprint, the apply path, and the cache that sits in front of the fold. The
query path gets only the small follow-ups the fast-path plan's Layer 2
stopped short of.

Touches `src/adapter/net/behavior/fold/{expiry,state,mod,capability,capability_bridge}.rs`
and `benches/net.rs`. All paths below are relative to `net/crates/net/`.

## The gap

The fold's read side is already fleet-shaped. The recorded run in
`benchmarks/BENCHMARK_RESULTS_14900K_2.md` shows the fixed-cardinality
`capability_fold_scaling/query_tag_rare` bench holding flat at ~2.8 µs from
5k to 50k nodes, and the non-selective `query_tag` growing only with its
result set (1.2 ms for ~25k matches at 50k nodes, ~48 ns per match). The index
is not the problem. Four other things are, and none of them is visible in the
benchmarks that exist today.

### 1. The expiry sweep is O(fleet) per tick and restarts its walk per chunk

`sweep_expired` (`src/adapter/net/behavior/fold/expiry.rs:65`) walks the whole
`entries` map under the read lock every 500 ms (`DEFAULT_SWEEP_INTERVAL`,
`expiry.rs:35`) whether or not anything is expired. Worse, the chunking at
`expiry.rs:78-88` restarts the walk from the first bucket for every
1024-entry chunk:

```rust
let candidates: Vec<K::Key> = {
    let state = state_lock.read();
    state.entries.iter()
        .filter(|(_, e)| e.expires_at <= now)
        .map(|(k, _)| k.clone())
        .take(SWEEP_CHUNK_SIZE)
        .collect()
};
```

Two different costs hide in that restart, and they scale differently:

- **Live entries yielded again.** Live entries in the prefix that earlier
  chunks already walked are yielded again by every later chunk. With N
  entries, E of them expired and spread uniformly, chunk k reaches roughly
  position k·1024·N/E. The live entries yielded across the whole sweep come
  to about (N − E)·E/2048. At N = 1M and E = 100k (a partition heals, or a
  wave of nodes goes quiet together) that is ~4.4·10⁷ yielded live entries
  across ~100 read-lock acquisitions.
- **Emptied storage scanned again.** Entries removed by earlier chunks are
  not yielded again; the iterator scans past their emptied slots, and that
  costs control-byte group scans proportional to table capacity, not entry
  visits. In a whole-fleet expiry (E = N), each entry is yielded once. The
  repeated cost there is capacity scanning, which this plan does not
  estimate.

Both costs are arithmetic from the loop's shape. Slice 0 measures them
separately, as yielded entries and as scan work against table capacity, and
those measurements replace these figures. The steady-state cost is a full
walk of `entries` twice a second that finds nothing.

`sweep_evicts_across_multiple_chunks_when_count_exceeds_chunk_size`
(`fold/tests.rs:1328`) proves the chunking is correct; nothing proves it is
cheap.

### 2. Memory, not nanoseconds, is what stops a million-node fold

Per node the payload carries ~35 tags as `Vec<String>`
(`CapabilityMembership::tags`, `capability.rs:87`), roughly 1.5 KB. The
inverted index holds one 16-byte `(u64, NodeId)` key per tag per node in a
`HashSet` (`CapabilityIndexInner::by_tag`, `capability.rs:365`). That is
560 bytes per node for `by_tag` alone, before `HashSet` control bytes and
spare capacity.

At 1M nodes that is on the order of 1.5 GB of payload strings plus ~700 MB
of index keys. That is before counting `by_synthetic`, `by_region`,
`by_state`, the primary map and the reverse index. The strings repeat
heavily across nodes: `hardware.gpu.vram_gb=80` is the same bytes on every
such node. Each node stores it once in its payload and once more as a clone
in the `by_tag` map key.

Nothing measures bytes per entry today. These figures are arithmetic from
the struct shapes. Slice 0 replaces them with a measurement, split into
payload, index, primary/reverse-map and (later) interner bytes.

### 3. The apply path allocates under the write lock on every accepted announcement

Four places, while `state` and `index` are write-locked (`Fold::apply`,
`mod.rs:363-364`), plus one upstream of the lock:

- `CapabilityIndexInner::on_insert` (`capability.rs:399`) does
  `self.by_tag.entry(tag.clone()).or_default()`: a `String` allocation per
  tag even when the bucket already exists, which in steady state it always
  does.
- `derive_synthetic_index_tags` (`capability.rs:511`) runs `Tag::parse` on
  every tag, which allocates `key` and `value` strings for every axis tag
  (`tag.rs:257-279`), then `format!`s the synthetic key. It runs on insert and
  again on remove.
- The generic Replace arm (`mod.rs:408-413`, `:429-433`) removes the old key
  from `by_node` and deletes the per-node `HashSet` once it is empty. It
  then recreates that set and inserts the same key. With one key per
  publisher, that is an allocation on **every accepted refresh**, even when
  `index_payload_equivalent` skips the secondary-index churn.
- Upstream of the lock, `translate_announcement`
  (`capability_bridge.rs:1255`) re-renders every tag with `to_string` and
  re-parses the hardware view for every inbound announcement.

The `index_payload_equivalent` fast path (`capability.rs:483`) skips the
index churn on a steady-state refresh, so the first two only bite on first
sight and real changes. The `by_node` recreation and the translate cost bite
on every announcement.

At the plan's operating range this matters: every node ingests every other
node's announcement, so at 1M nodes and the default 150 s re-announce
interval (`MeshNodeConfig::capability_reannounce_interval`, `mesh.rs:3698`) a
node applies ~6.7k announcements per second. Ed25519 verification dominates
that budget and is out of scope here, but the fold's share should be
allocation-free on the warm refresh path.

The existing `capability_fold_insert` bench (`benches/net.rs:2304`) cannot see
any of this. It builds `sample_capability_set` and a `CapabilityAnnouncement`
inside `b.iter`, so its ~31 µs per node is mostly fixture construction. There
is no apply-only bench, no refresh bench, no translate bench, no sweep bench,
and no footprint measurement.

### 4. The capability-set cache is invalidated by every announcement in the fleet

`CapabilitySetCache` (`capability_bridge.rs:807`) keys hits on the fold's
single change generation (`Fold::change_generation`, `mod.rs:331`), and
`Fold::apply` bumps that generation on every accepted Insert and Replace
(`mod.rs:386`, `mod.rs:442`). That includes the refresh case where
`index_payload_equivalent` already proved nothing indexed changed. At 6.7k
applies per second the generation moves every ~150 µs. A cached set then
survives only as long as no publisher anywhere in the fleet re-announces.

What that costs, by consumer:

- **The cached consumer is per-packet greedy admission** (`mesh.rs:32220`).
  It goes through `get_or_synthesize` and loses its hits to fleet-wide
  churn. This is the path Track D fixes.
- **`StandardPlacement::placement_score` does not use the cache.** It calls
  the uncached `synthesize_capability_set_if_known` (`placement.rs:544-545`),
  so Track D does not change its cost.
- **Coherent bulk selection deliberately bypasses the cache.**
  `best_node_matching` and `candidates_for_selection`
  (`capability_bridge.rs:788-795`, `:1743-1757`) synthesize inside the
  snapshot that admitted their candidates. They must not be rerouted
  through the cache, because a cache lookup re-enters the fold from an
  already-held coherent snapshot.

The cache's own doc comment says "announcement rates are low relative to
scoring/per-packet rates, so the cache stays warm in steady state." That is
true at hundreds of nodes and false at hundreds of thousands. Separately, its
default capacity is 256 entries (`capability_bridge.rs:774-777`), so capacity
misses and coherence misses must be told apart before any hit-rate claim is
made.

### 5. Query-path leftovers

Smaller, and listed last because the fast-path plan already took the big
wins:

- `build_group_unions` / `group_union` (`capability.rs:850-884`) materialize
  an owned `HashSet` for every `tag_groups_all` group, including a
  single-tag group such as `gpu:present` that may cover the whole fleet.
  That is the visible gap between `query_complex` (2.7 ms) and `query_tag`
  (1.2 ms) at 50k nodes in the recorded run.
- The seed at `capability.rs:754` is always `tags_all` when present, even if
  a model group is far more selective, and `resolve_keys_all_tags` clones the
  seed bucket into an owned set (`capability.rs:652`) before `retain`ing
  through the rest.
- `FoldState::entries` and `by_node` (`state.rs:176`, `state.rs:185`) use the
  std SipHash default while the index sets use the Fx mixer, so every
  per-candidate `entries.get` in materialization pays SipHash on a key that
  is already a digest.

## Operating point

The plan's finish line is **1M publishers at the default 150 s re-announce
interval, with the full fold held on server-class nodes.** That is ~6.7k
applies per second per receiving node. Every slice's proof is ultimately
read against that figure.

What the plan cannot move, even with every slice landed: every node that
holds the full fold still ingests every other node's announcement. At the
operating point and ~1.5 KB per announcement, that is ~10 MB/s (~80 Mbit/s)
of announcement traffic into each such node. Each announcement also costs
one Ed25519 verification, which is out of scope (see below). Both costs are
O(fleet) per node by construction.

Beyond the operating point, or for nodes that cannot afford that ingest
(browser leaves, mobile, constrained devices), the answer is not more fold
optimization. It is not holding the full fold at all: aggregators hold it
and leaves query them (`behavior/aggregator/`). A follow-up plan that pushes
past 1M starts there, not here.

## Supported semantics this plan must preserve

The review surfaced contracts that an optimization could break silently.
Every track is held to them:

- **The indexed unit is the (class, node) entry, not the node.**
  `CapabilityMembership` permits one announcement per (class, publisher)
  pair (`capability.rs:74-87`), and typed signed-fold dispatch accepts any
  class (`dispatch.rs:105-123`). Queries keep class predicates
  (`capability.rs:798-800`), and
  `find_nodes_matching_dedupes_publisher_across_classes`
  (`capability_bridge.rs:2402-2415`) exercises two entries for one
  publisher. A conjunctive query matches only when a **single entry**
  satisfies every predicate. If class A of node N carries `alpha` and class
  B carries `beta`, a query for both matches nothing. Publishers are
  deduplicated only after entry-level predicates are satisfied. One legacy
  producer that emits a single class is not evidence that the fold supports
  only one.
- **Results are ordered by `NodeId`.** Deterministic placement
  (`capability_bridge.rs:1379-1385`) and selection tie-breaking (`:1733-1741`,
  `:1808-1812`) depend on numeric `NodeId` order. Any representation
  that changes iteration order (dense slots, bitmaps) keeps the explicit sort
  unless it proves equal output under arrival order, churn and restore.
- **The tag grammar is `Tag::parse`'s.** `parse_axis_body` (`tag.rs:422-447`)
  accepts both `=` and `:` after any taxonomy axis and splits at whichever
  comes first, so `software.model.0.id:llama3` derives `model:llama3` today.
  A performance slice does not change accepted grammar.
- **Snapshots are trusted local state.** `build_entry` keeps the payload,
  timing and generation, not the signed envelope (`mod.rs:808-823`).
  Snapshots are explicitly not verified publisher artifacts
  (`mod.rs:535-547`). Nothing in this plan adds signed-byte retention, and no
  test may demand that a snapshot re-verify a signature it never held.

## Design

Five tracks. Each is shippable on its own and each has a measurement that
would catch it being wrong. Order is by payoff per risk. The only hard
dependency is that Slice 0 lands first, so every later slice has a before
number. Tracks B (interning, bitmaps) and A2 (expiry wheel) are **held**
until their open contracts below are decided and re-reviewed.

### Track A: expiry without the restart penalty

**A1: single-pass candidate collection.** Walk `entries` once under the read
lock and collect every key with `expires_at <= now` into a `Vec`. Then evict
that list in `SWEEP_CHUNK_SIZE` slices, each under its own write-lock
acquisition, with the existing per-key re-check. That removes the
per-chunk restart: no live entry is yielded more than once per sweep, and no
emptied prefix is rescanned. It adds no new structure and no new invariant.
The write-lock hold per chunk is unchanged.

The cost is one read-lock hold for the whole walk instead of ~100 shorter
ones. That hold is a latency tail for **both** writers and readers.
`parking_lot`'s `RwLock` is fair, so once an apply queues for the write lock,
new readers queue behind it too. Queries do not proceed freely during the
walk. Slice 2 measures the writer and reader tail at 1M entries.

If the tail is too long, A1 does not get an ad-hoc fix. A std `HashMap`
iterator gives no cursor that survives dropping the guard. Recreating an
iterator and skipping an offset would bring the prefix rescans back. A
chunked read walk therefore needs a concrete bounded traversal designed and
reviewed at that point, for example a snapshot of keys taken under one
short hold, or a structure with a stable cursor.

A1 does nothing for the steady state: the walk still runs twice a second
and still finds nothing. That is why A2 exists, and why A2 is conditional on
measurement.

**A2: time-ordered expiry (held, conditional).** A side structure ordered by
deadline, maintained wherever `expires_at` is set or an entry is removed:
`Fold::apply` (Insert and Replace), `Fold::evict_node`, `Fold::restore`, and
the sweep itself. It lives inside `FoldState<K>`, under the existing `state`
lock and the existing `state → index` order.

Shape: buckets keyed by whole seconds since the fold epoch,
`BTreeMap<u64, HashSet<K::Key>>`. Two contracts the first draft left
unspecified:

- **Deadline rounding.** A key is placed in bucket `ceil(expires_at)`.
  Floor placement can lose an expiry permanently. With a deadline at
  10.75 s placed in bucket 10, a sweep at 10.5 s pops the bucket, the exact
  `expires_at <= now` re-check fails, and dropping the "stale" slot discards
  the only future expiry record. With ceiling placement, every key in a
  popped bucket is already due. The stated cost: an entry can outlive
  `expires_at` by up to 1 s plus one sweep interval (1.5 s at the default).
  That lag is part of the contract and is documented on `FoldEntry`. A
  partial-bucket alternative, popping a bucket but keeping its not-yet-due
  keys, is also acceptable if chosen instead. The choice must be explicit.
- **One active placement per key.** `FoldEntry` records the bucket second
  it is placed in. A refresh that moves the deadline into a different bucket
  removes the key from its old bucket and inserts it into the new one,
  inside the same write-locked apply. There are no lazy stale slots, so
  retained slots never exceed live entries. The invariant is exact:
  `sum(bucket sizes) == entries.len()`, checked by a `debug_assert` after
  every mutation path in tests. The first draft's lazy-slot policy kept
  refresh history the same way a lazy heap does. One live entry first
  scheduled at 300 s and refreshed at 150 s to 450 s held two slots, and
  its proposed assertion failed on that ordinary schedule.

Bucket sets are `HashSet`s, not `SmallVec`s. At 1M entries over a 150 s
cycle, a bucket holds ~6.7k keys, and the per-refresh move must not be a
linear scan.

The sweep drains, in order, every bucket whose second is at or below
`floor(now)`. It does **not** pop a whole bucket and then drain it
across unlocked chunks. That would leave undrained keys live in `entries`
with no bucket placement, breaking the exact-sum invariant between chunks.
Instead, each chunk does all of this in one write-locked critical section:

- take up to `SWEEP_CHUNK_SIZE` keys out of the bucket;
- remove those keys from `entries`, `by_node` and the index;
- drop the bucket's map entry only when it is empty.

Keys not yet drained stay in their bucket. The invariant therefore holds at
every lock release, not only at the end of the sweep. The chunked hold
discipline stays.

Why not `BTreeMap<Instant, Key>` with exact instants: every apply pays a
tree insert into a map as large as `entries`. Why not a heap: a refresh
cannot cheaply move an entry, and lazy deletion keeps refresh history, not
the fleet.

### Track B: intern tags, then bitmap the buckets (held)

Both steps are held until the decisions below are written into this plan and
re-reviewed. The memory problem in Gap §2 is real; the first draft's
mechanism for it was not specified tightly enough to implement.

**B1: tag interning.** The goal is unchanged: store each distinct tag string
once per fold, and key the tag indices on a `u32` id instead of a cloned
`String`. What must be decided first:

- **The representation and conversion seam.** `FoldKind::Payload` is one
  type serving signed dispatch, stored entries, query output and the
  snapshot payload (`mod.rs:107-109`, `state.rs:131-133`, `dispatch.rs:105`,
  `snapshot.rs:38-43`). A wrapper that keeps the original payload keeps its
  strings. Replacing the payload with ids needs a defined conversion and
  serialization context. Pick the smallest seam that works, and say where
  conversion happens for **both** intake paths: legacy
  `translate_announcement` and typed signed-fold dispatch
  (`dispatch.rs:105-123`). Then say where it happens for trusted
  `restore`. Interning only in `translate_announcement` misses typed
  dispatch.
- **Reader access.** The public scoped-query callback consumes `&[String]`
  from the admitting snapshot (`capability_bridge.rs:1548-1552`,
  `:1585-1604`). Resolving ids to `Arc<str>` is not a transparent
  replacement for that signature, and one `Arc` clone per tag is not free.
  Decide how readers get coherent, cheap string access, and record any
  public source-compatibility change.
- **Compatibility.** Wire compatibility and source compatibility are
  separate. The wire form of `CapabilityMembership` does not change, so the
  original wire encoding still decodes and verifies. Local snapshot
  schema/semantic compatibility is defined separately (snapshots are
  trusted local state). Tag order and content are preserved exactly. The raw
  and synthetic tag namespaces stay separate.
- **A lifetime memory budget, with defined exhaustion.** Tags are
  publisher-chosen strings. A per-announcement cap on tag count and length
  plus a per-publisher cap bounds only a fixed population. A dictionary that
  never forgets keeps tags across sequential publisher churn, so
  `publishers × cap` must count every historical publisher, not just live
  ones. The bound is therefore an **aggregate retained-dictionary budget**,
  in bytes and in ids, with explicit behavior at exhaustion. The
  recommendation is reference-counted ids, released when the last entry
  holding a tag goes away, so retained size tracks live tags. The
  alternative, a never-forgetting dictionary that rejects new tags at
  exhaustion, is acceptable only if that rejection behavior is acceptable
  for the life of the fold. Rejected intake must not allocate outside the
  budget.

  No capability-tag cap exists today: the only tag cap under `behavior/` is
  `MAX_METADATA_TAGS` (`metadata.rs:394`), which does not cover
  `CapabilityMembership`. The per-announcement cap lands with B1 regardless,
  enforced before interning. A `u32` id ceiling is not an operational
  memory budget.

**B2: bitmap buckets.** The goal is word-level AND/OR and a footprint of a few
bits per membership, replacing the per-tag `HashSet` of 16-byte keys. What
must be decided first:

- **Bitmap identity is the (class, node) entry.** Dense `u32` slots are
  allocated per entry, not per node. Node-level bitmaps would wrongly match a
  tag in class A combined with a tag in class B; see "Supported semantics".
  Predicates are evaluated per entry. Publishers are deduplicated after.
- **Ordering is kept by an explicit sort.** Roaring yields sorted slot
  numbers, and slots are assigned in arrival order, so slot order is not
  `NodeId` order. Allocating slots for nodes `[90, 10]` yields `[90, 10]`,
  while the API returns `[10, 90]`. The `sort_unstable` + `dedup` at
  `capability_bridge.rs:1384` and `:1614` stays, unless the slice proves
  another representation gives identical output across arrival order, churn
  and restore.
- **Slot reclamation.** A slot freed by expiry or eviction is either reused
  through a free list or counted against a stated bound.
- **The `roaring` dependency.** A new pure-Rust crate in the core. The risk
  is review bandwidth, not the build.

Targets for B1 and B2 are set separately, from Slice 0's measured
breakdown. B1 alone cannot remove the index tuple sets: at the plan's own
35 tags × 16 bytes, `by_tag` keeps 560 bytes per node until B2. So no whole-fold
bytes-per-entry target is set for B1 alone without first subtracting the
index bytes B1 does not touch.

### Track C: allocation-free apply

Independent of B, and smaller:

- **`on_insert`:** probe `get_mut` first, and fall back to `insert` only on
  a miss.
- **`by_node` on Replace:** when the same owner replaces the same key,
  leave the reverse-index membership in place instead of removing it,
  dropping the empty set and recreating it. The **`keys` field** of
  Slice 1's per-node record becomes `SmallVec<[K::Key; 1]>` with linear
  membership checks. The record itself, and its `rev`, stays. Replacing the
  whole `by_node` value with a bare `SmallVec` would drop the publisher
  revision. An accepted Replace still advances `rev` even when reverse-index
  membership is left unchanged: the payload changed, so the cached set must
  miss. This change is pulled into this track from the old query-leftovers
  track, because Slice 3's allocation gate cannot pass without it.
- **`derive_synthetic_index_tags`:** replace `Tag::parse` with a
  borrowing extraction that returns `&str` slices and **matches
  `parse_axis_body`'s grammar exactly**: split at the first of `=` or `:`,
  with the same presence/value split. Splitting only at `.id=` is wrong; it
  would miss `software.model.0.id:llama3`. The synthetic key is written into
  a reused buffer. Borrowing the parse does not by itself remove the
  allocation of the synthetic output; that is counted separately.
- **`translate_announcement`:** build the hardware summary and the tag
  list in one pass over `ann.capabilities.tags`, instead of `views()` plus a
  `to_string` map.

The allocation claim is narrowed to a measured boundary. Zero allocations
inside `Fold::apply` on a **warm, index-equivalent refresh**: the same owner
and key, an already-resident entry, and no table growth. Changed-index
replacements and cold table growth are reported, not gated at zero.

**Removal path (added after Slice 0).** Slice 0 measured removing one entry
at 9–12 µs, more than an insert. Track C therefore also benchmarks and
optimizes the removal side:

- `on_remove`;
- the borrowing synthetic derivation on removal;
- the cost of destroying or retiring the payload.

Primary, reverse and index consistency, audit and watch behavior, and
publisher-revision maintenance are preserved. The zero-allocation gate
above does **not** extend to removal. A cold removal frees the entry's
allocations by definition, so no "no deallocation" promise is made.
Removal is reported against its Slice 0 baseline.

### Track D: publisher-revision cache validity

The cache stores a node's synthesized set, and synthesis merges **every**
class entry that node owns (`capability_bridge.rs:746-770`). A per-entry
stamp is therefore the wrong token. Reading one entry's stamp misses changes
to that node's other classes.

Even the maximum stamp across a node's live entries fails. Cache a node
whose classes carry stamps 5 and 9, then expire the stamp-5 class. The
synthesized set changes, but the maximum is still 9, so the cache serves a
stale hit. That is a gap in the first draft's design, not a defect in
today's global-generation cache.

Design: a **receiver-local publisher mutation revision**. `by_node`'s
per-node record carries a `rev: u64`, drawn from a fold-wide monotonic
counter, and is advanced by every mutation that can change that node's
synthesized set:

- an accepted Insert or Replace for **any** of its classes;
- expiry or removal of any one of its classes, including a partial expiry
  that leaves other classes resident;
- `evict_node`;
- `restore`, which allocates a **fresh** revision for every restored node.
  Revisions are never restored from a snapshot, so a pre-restore cache entry
  can never validate against post-restore state.

When a node's last entry goes away, its record is removed. If it
reappears, it draws a new revision from the monotonic counter, so
absent-then-present can never reuse a revision (no ABA).

`get_or_synthesize` keys hits on `(node_id, rev)`. It reads `rev` in the
same state borrow it already takes for synthesis
(`synthesize_capability_set_if_known`), so no second lock is added. The
existing ordering argument carries over: read the revision, then
synthesize, in that borrow. A concurrent apply to the same node therefore
causes one miss, never a stale serve.

Owner-projection mutations: the retraction through `with_state_mut`
(`mod.rs:691`) changes the owner projection in place. Synthesis reads tags
and metadata only, not the owner projection, so that retraction does not
change the cached value and does not need to advance `rev`. Ownership
retraction alone is therefore not a stale-capability bypass. If synthesis
ever starts reading the projection, `with_state_mut` callers must advance
`rev`, and that requirement is documented at the synthesis function.

Scope: this fixes the cached consumer, greedy admission. It does not change
`placement_score`, which is uncached, or coherent bulk selection, which is
deliberately uncached. Neither is rerouted through the cache.

Decision, with a recommendation: whether `Fold::apply` should also skip
`signal_changed` when the old and new payloads are byte-equal, not merely
index-equivalent. Recommendation: no, not in this plan.
`subscribe_changes_fires_on_real_mutations_only` (`fold/tests.rs:173`) pins
that a Replace signals. The `watch_tools` consumer at `mesh.rs:34879`
already debounces, and D alone removes the cache cost. A watch-semantics
change belongs in its own plan with the watcher owners in the room.

### Track E: query leftovers

**E1: cheap query fixes (pulled forward).** Neither needs interning or
bitmaps:

- `group_union` returns `CandidateKeys::Borrowed` for a single-tag group and
  materializes only multi-tag groups.
- Seed selection picks the smallest bucket across `tags_all`, every group,
  state and region. It then streams: iterate the seed, probe the others,
  and collect into a `Vec` with no intermediate owned set.

Output order and content are unchanged: the final `NodeId` sort and dedup
stay where they are.

**E2: the hasher seam.** `FoldKind` gains a **required** associated type,
`type KeyHasher: BuildHasher + Default`, used by `FoldState::entries` and
`by_node`. `CapabilityFold` sets it to `BuildU64TupleHasher`. Routing,
reservation and every other existing `FoldKind` impl set `RandomState`
explicitly. A defaulted associated type (`= RandomState`) is not an option:
associated type defaults are unstable (E0658) on the pinned Rust 1.99.0.

Adding a required associated type is a source-breaking change to the public
`FoldKind` trait for any out-of-tree implementor. It is recorded as such in
the release notes, or `FoldKind` is confirmed crate-private first.

## The slices

### Slice 0: measure before touching anything

Delivers the benches and probes every later slice reports against. The
first draft's benches would have measured rejected applies, consumed state
or fixture construction. Every bench below states its preparation and reset
and asserts its outcome. Preparation runs outside timing; only the operation
is timed.

**Resets must interleave with measured operations.** `iter_batched` alone
does not guarantee that: Criterion prepares every input in a batch before
running any routine. On the locked Criterion 0.8.2, the review measured
what happens with a four-input batch over one shared fixture:

- insert resets produced `Inserted, Replaced, Replaced, Replaced`;
- expiry resets produced eviction counts `10, 0, 0, 0`.

So a bench whose setup resets a **shared** fixture uses one of three forms:

- `BatchSize::PerIteration`, which gave four inserts and four ten-entry
  sweeps in the same check;
- a fully independent fixture per input;
- controlled custom timing (`iter_custom`) that resets and then times each
  operation in turn.

The per-iteration outcome assertions below are what catch a regression to
batched shared resets.

- `capability_fold_apply/{insert,refresh_equivalent,replace_changed}` at
  10k, 100k and 1M resident entries.
  - `Fold::apply` consumes its envelope, and rejects a generation no newer
    than the resident one (`mod.rs:136-144`, `:350`). So every iteration
    gets a **fresh owned envelope** with a strictly increasing generation,
    built in untimed setup. `replace_changed` alternates between two payloads
    that differ in an indexed field.
  - `insert` keeps residency fixed: untimed setup removes the target entry,
    and the timed operation re-inserts it, so the fold never grows across
    iterations.
  - Every iteration asserts its `ApplyOutcome` (`Inserted` or `Replaced`,
    never `Rejected`). A bench that silently measures rejection fails
    instead of reporting a fast number.
- `capability_translate/announcement`: `translate_announcement` alone, on
  prepared announcements. Apply-only timing excludes the upstream work
  Track C also changes.
- `capability_fold_sweep/{steady,mass_expiry}` at 100k and 1M entries.
  Untimed setup **re-arms** the expired population before every iteration:
  10% expired for `mass_expiry`, 0% for `steady`. A sweep therefore never
  measures a population an earlier iteration already consumed. Each
  iteration asserts its eviction count. Two numbers are reported
  separately: yielded entries, and scan work against table capacity
  (Gap §1). The 1M `steady` result decides whether Slice 8 is built.
- `capability_set_cache` workload. The cache capacity, hot-set
  distribution, lookup rate and mutation rate are stated in the bench, not
  implied. At minimum it runs one configuration with a hot set at or below
  capacity (default 256) and one larger. Mutations come from publishers
  inside and outside the hot set at the operating point's per-publisher
  rate. **Coherence misses and capacity misses are reported separately**,
  which needs a split miss counter on the cache. No hit rate is
  predeclared for either the current or the new cache.
- `capability_fold_footprint`: not a Criterion bench. It is a
  `#[test] #[ignore]` under `fold/tests.rs` using a counting global
  allocator, run by hand with
  `cargo test --lib --features "$UNIT_FEATURES" -- --ignored fold_footprint --nocapture`.
  - It reports **retained** bytes (live after the build) separately from
    **cumulative** allocation.
  - Retained bytes are broken down into payload, index, and primary plus
    reverse maps.
  - It runs at 100k entries in three configurations: fixture repetition;
    a mixed fixture where a fixed fraction of each node's tags is unique;
    and a churn run in which a share of publishers leaves and is replaced.
    Interning's payoff depends entirely on repetition and churn, so a single
    favorable fixture would overstate B1.
- `capability_fold_mixed`: one controlled concurrent workload at 1M
  residency. Applies run at the operating point's ~6.7k/s, with queries and
  the background sweep alongside. It reports accepted apply throughput, apply
  and query latency (p50/p99/p999) and lock-hold tails. Isolated operation
  tables do not establish the concurrent operating point. This stays a fold
  workload; transport and crypto are not modelled.

Proof: the numbers land in this file's Status section with the command,
the commit and the machine. Slice 0 is done when every table exists.

### Slice 1: publisher-revision cache validity (Track D)

First after measurement: one `u64` per node record and a different cache
key, fixing the cached greedy-admission path, which loses its hits to
fleet-wide churn well before fleet scale.

Delivers the per-node revision, its maintenance on every mutation path, and
the re-keyed cache.

Proof: the existing cache tests at `capability_bridge.rs:2867-3003` stay
green. On the Slice 0 cache workload, coherence misses caused by **other**
publishers' announcements drop to zero; capacity misses are reported
unchanged. New witnesses:

- `cache_survives_other_publishers_announcements`: cache node A, apply a
  refresh from node B, and assert A hits. Apply a refresh from A, and
  assert A misses once.
- `cache_misses_on_sibling_class_change`: node A with two classes; change
  one; assert the cached set misses.
- `cache_misses_on_older_sibling_expiry`: node A with classes written at
  different times; expire the older one; assert a miss (the max-stamp
  counterexample).
- `cache_unknown_to_known`: a miss before A's first announcement, then a
  correct set after it.
- `cache_misses_after_eviction_and_reannouncement`: evict A, re-announce
  it, and assert the revision is new and the set is fresh.
- `cache_misses_after_forced_restore`: populate, restore a snapshot, and
  assert the first lookup misses.
- `cache_concurrent_miss_populate_never_serves_stale`: a scheduled race
  between an apply to A and `get_or_synthesize(A)`. Assert that no lookup
  after the apply returns the pre-apply set.

### Slice 2: single-pass expiry sweep (Track A1)

Delivers one read-locked walk that collects every expired key, then chunked
write-locked eviction with the existing re-check.

Proof: on `capability_fold_sweep/mass_expiry`, the count of yielded live
entries drops from ~(N − E)·E/2048 to at most N. Scan work drops to one pass
over capacity. `steady` is unchanged, by design. The three existing sweep
tests at `fold/tests.rs:1248`, `:1315`, `:1328` and
`background_sweeper_evicts_expired_entries_on_tick` stay green.

New witness `mass_expiry_yields_each_entry_once`: 100k entries, 10% expired.
Assert the sweep's yielded-entry counter (exposed on `FoldMetrics` for this
purpose) is at most the entry count captured **before** the sweep. After
eviction, `entries.len()` has shrunk by the evicted 10%, so it would be the
wrong bound.

The writer and reader tail during the single read hold is measured on
`capability_fold_mixed` at 1M and recorded. If it is unacceptable, the fix
is the designed traversal described under A1, not an ad-hoc resume.

### Slice 3: allocation-free apply (Track C)

Delivers the `get_mut`-first insert, the same-key `by_node` preservation
with the `SmallVec` value, the grammar-exact borrowing synthetic
derivation, and the single-pass translate.

Proof:

- `capability_fold_apply/insert` and `replace_changed` improve, and
  `capability_translate/announcement` improves.
- An allocation-counting assertion in the apply bench pins **zero
  allocations inside `Fold::apply` on a warm index-equivalent refresh**
  (same owner, same key, resident, no growth). Other paths report their
  counts.
- The agreement test at `capability_bridge.rs:2457`, the
  synthetic-tags-stay-index-only test, and
  `find_nodes_matching_dedupes_publisher_across_classes`
  (`capability_bridge.rs:2402`) stay green, as do all of Slice 1's cache
  witnesses.
- New witness `same_key_replace_advances_rev_without_touching_membership`:
  an accepted Replace from the same owner and key leaves the `keys` field
  unchanged and makes the cached set miss once.
- New differential test `synthetic_derivation_matches_tag_parse`. It
  compares the borrowing extraction with the `Tag::parse` path over: both
  separators (`id=x`, `id:x`); a value with embedded delimiters (`id=a:b`,
  `id:a=b`); empty values; bundle-key shapes; and raw tags that collide with
  synthetic names.

### Slice 4: cheap query fixes (Track E1)

Pulled ahead of the representation rewrite, if Slice 0 shows the
`query_complex` vs `query_tag` gap is still there.

Delivers borrowed single-tag groups and selectivity-ordered streaming
intersection.

Proof: `capability_fold_scaling/query_complex/50000` narrows against
`query_tag/50000`, and `query_tag_rare` stays flat. Output is identical to
the pre-slice output on the 10k fixture. That comparison test, including
cross-class and opposite-arrival-order cases, stays in the suite
permanently as a regression witness; it is not deleted after the PR.

### Slice 5: hasher seam (Track E2)

Delivers the required `KeyHasher` associated type on `FoldKind`, with an
explicit choice in every existing impl.

Proof: the existing `capability_fold_query` and `capability_fold_scaling`
benches show the materialization cost of `entries.get`. The full fold unit
surface stays green. The `FoldKind` source-compatibility note is in the
release notes, or the trait is confirmed crate-private.

### Slice 6: tag interning (Track B1) — held

Not authorized until Track B1's four decisions (seam, reader access,
compatibility, lifetime budget) are written into this plan and re-reviewed.

Proof, once authorized:

- Retained interner bytes and ids stay within the stated budget under the
  churn footprint run.
- Exhaustion behaves as specified, with no allocation on rejected intake.
- Retained payload bytes drop against a target set from Slice 0's
  breakdown. The target is for payload bytes, not the whole fold.
- Both intake paths and trusted `restore` produce identical entries.
- Original-wire announcements still decode and verify.
- Local snapshot round trip preserves tag order, content and namespace.
- The full fold unit surface, `org_routing_wiring_tests` and the
  `cross_lang_*` suites stay green.

### Slice 7: bitmap buckets (Track B2) — held

Not authorized until Track B2's decisions (entry-level identity, ordering,
slot reclamation) are written into this plan and re-reviewed.

Proof, once authorized:

- New witness `cross_class_split_predicate_does_not_match`: class A of node
  N has `alpha`, class B has `beta`, and a query for both returns nothing.
- New witness `output_order_independent_of_arrival`: the same population
  inserted in opposite orders, plus churn and a restore, gives identical
  output.
- Slice 4's permanent comparison witness stays green.
- Retained index bytes drop against a separate B1+B2 target.

### Slice 8: time-ordered expiry (Track A2) — held, conditional

Built only if Slice 0's `capability_fold_sweep/steady` at 1M entries, or a
profile of a production-sized fold, shows the twice-a-second empty walk
costs more than the wheel's invariant is worth. Otherwise this slice is
dropped.

Delivers the per-second buckets with ceiling placement (or the explicitly
chosen partial-bucket alternative) and one active placement per key,
maintained by apply, evict, restore and the sweep.

Proof:

- `steady` drops from a full-map walk to microseconds, and `mass_expiry`
  holds Slice 2's number or better.
- The `sum(bucket sizes) == entries.len()` assertion holds after every
  mutation path in tests, and at every lock release inside a multi-chunk
  sweep, not only at its end.
- New witness `multi_chunk_drain_keeps_placement_invariant`: one due bucket
  larger than `SWEEP_CHUNK_SIZE`; assert the exact-sum invariant between
  chunks, and that undrained keys remain in their bucket.
- New witnesses:
  - `partial_second_deadline_is_never_lost`: a deadline at x.75 s and a
    sweep at x.5 s; the entry expires by the stated lag.
  - `refresh_before_old_deadline_moves_placement`: one slot before the
    refresh, one slot after, in the new bucket.
  - `evicted_entry_leaves_no_slot`.
  - `first_sweep_after_restore_evicts_exactly_elapsed`.
  - `idle_sweep_touches_no_live_entry`.
  - `sweep_cost_is_independent_of_live_entry_count`: two folds, 1k and
    100k live entries, the same 100 expired; the visit counter is equal.
- Refresh bursts and long TTLs are exercised against the same invariant.

## Risks

- **The cache revision misses a mutation path.** A path that changes a
  node's entries without advancing `rev` serves a stale set. Fallback: every
  path that touches `by_node` advances `rev` at one choke point, the
  per-node record update, not at each caller. Slice 1's witness list covers
  every lifecycle transition named in Track D.
- **The single read hold of A1 lengthens tails.** Writer and reader tails
  are measured at 1M. Fallback: a designed bounded traversal, re-reviewed;
  never an offset-resume over a recreated iterator.
- **The synthetic derivation drifts from `Tag::parse`.** Fallback: the
  differential test in Slice 3 runs on every change to either function.
- **The expiry wheel (if built) desynchronizes from `entries`.** With one
  placement per key, desync is a counting bug, not a memory leak, and the
  exact-sum assertion catches it in tests. `restore` is the path most likely
  to be forgotten; its witness asserts the first sweep after restore evicts
  exactly the entries whose TTL elapsed.
- **The interner's budget is breached by publisher churn** (if B1 is built).
  The aggregate budget covers historical publishers by construction, and
  the churn footprint run measures it. A gauge (`FoldStats::interned_tags`
  and retained bytes) shows it to operators. The gauge is not the bound.
- **The `KeyHasher` change breaks an out-of-tree `FoldKind`.** Fallback:
  confirm `FoldKind` is crate-private, or ship the change with a release
  note.
- **A benchmark measures the fixture, a rejection, or consumed state.**
  Fallback: the outcome assertions and untimed setup/reset in Slice 0, not
  a hardware-sensitive timing threshold.

## Not in scope

- **Ed25519 verification cost per announcement.** It dominates the ingest
  budget at scale and is independent of the fold. Batch verification is its
  own plan.
- **Changing the wire form of `CapabilityMembership`.** Tags stay
  `Vec<String>` on the wire; any interning is receiver-local. A compact tag
  codec is the deferred item in PERF_AUDIT_2026_05_28_CAPABILITY §"What was
  deferred".
- **Retaining signed announcement bytes in the fold.** Snapshots are trusted
  local state and keep no envelope today. This plan does not change that.
- **Making `placement_score` use the cache.** It is uncached today, and
  coherent bulk selection must stay inside its admitting snapshot. Neither
  is re-routed here.
- **Skipping the change signal on byte-equal refreshes.** A watcher-semantics
  change; see Track D's decision.
- **Merging the `state` and `index` locks, or replacing them with a
  snapshot structure.** Fewer lock operations per query, but the measured
  reads already parallelize (`capability_fold_concurrent`), and nothing in
  this plan changes the lock shape.
- **Routing and reservation folds.** Track A lands in the generic runtime
  and benefits them for free; Tracks B through E are capability-only, except
  E2's required `KeyHasher` choice, which every `FoldKind` makes.
- **The `DashMap`-backed graphs and the 40 ns capability check.** Both are
  design choices for the million-node target, not regressions.
