# Capability fold at fleet scale: expiry, memory, write path, cache

## Status

**Planned.** Written 2026-10-06 against `LZL0/scaling` at `806635756`. No
slice has landed. Targets the release after the next one; the first two slices
(Slice 0 measurement, Slice 1 per-entry cache validity) are the ones worth
pulling forward if the schedule tightens.

Successor to [`CAPABILITY_QUERY_FAST_PATH_PLAN.md`](CAPABILITY_QUERY_FAST_PATH_PLAN.md),
which made the bulk query path O(matches). This plan is about everything that
plan deliberately left alone: the expiry sweep, the per-entry memory
footprint, the apply path, and the cache that sits in front of the fold. The
query path gets only the two small follow-ups the fast-path plan's Layer 2
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

### 1. The expiry sweep is O(fleet) per tick and super-linear under mass expiry

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

With N live entries and E expired ones spread uniformly through the map,
chunk k restarts at the first bucket, re-walks the prefix the earlier chunks
already cleared, and stops at roughly position k·1024·N/E. Summed over the
E/1024 chunks, the sweep visits about **N·E/2048** entries: linear in E, not
logarithmic. At N = 1M, E = 100k (a partition heals, or a wave of nodes goes
quiet together) that is ~5·10⁷ entry visits across ~100 separate read-lock
acquisitions, on the order of 0.3–0.5 s of sweeper CPU. A whole-fleet expiry
(E = N = 1M) is ~5·10⁸ visits, a few seconds. The steady-state cost is a
full scan of `entries` twice a second that produces nothing: about 1% of a
core at 1M entries, under the read lock. These figures are arithmetic from
the loop's shape at ~5–10 ns per visit; Slice 0 replaces them with a
measurement.

`sweep_evicts_across_multiple_chunks_when_count_exceeds_chunk_size`
(`fold/tests.rs:1328`) proves the chunking is correct; nothing proves it is
cheap.

### 2. Memory, not nanoseconds, is what stops a million-node fold

Per node the payload carries ~35 tags as `Vec<String>`
(`CapabilityMembership::tags`, `capability.rs:87`), roughly 1.5 KB, and the
inverted index holds one 16-byte `(u64, NodeId)` key per tag per node in a
`HashSet` (`CapabilityIndexInner::by_tag`, `capability.rs:365`). At 1M
nodes that is on the order of 1.5 GB of payload strings plus ~700 MB of index
keys, before `by_synthetic`, `by_region`, `by_state`, and the primary map.
The strings are overwhelmingly repeated across nodes:
`hardware.gpu.vram_gb=80` is the same bytes on every such node, stored once
per node in the payload and once more as the `by_tag` map key's clone.

Nothing measures bytes per entry today. The figure above is arithmetic from
the struct shapes, and Slice 0 replaces it with a measurement.

### 3. The apply path allocates under the write lock on every accepted announcement

Three places, each paid per tag per announcement, while `state` and `index`
are write-locked (`Fold::apply`, `mod.rs:363-364`):

- `CapabilityIndexInner::on_insert` (`capability.rs:399`) does
  `self.by_tag.entry(tag.clone()).or_default()`: a `String` allocation per
  tag even when the bucket already exists, which in steady state it always
  does.
- `derive_synthetic_index_tags` (`capability.rs:511`) runs `Tag::parse` on
  every tag, which allocates `key` and `value` strings for every axis tag
  (`tag.rs:257-279`), then `format!`s the synthetic key. It runs on insert and
  again on remove.
- Upstream of the lock, `translate_announcement`
  (`capability_bridge.rs:1255`) re-renders every tag with `to_string` and
  re-parses the hardware view for every inbound announcement.

The `index_payload_equivalent` fast path (`capability.rs:483`) already skips
the index churn on a steady-state refresh, so the first two only bite on
first sight and real changes. The third bites on every announcement.

At the plan's operating range this matters: every node ingests every other
node's announcement, so at 1M nodes and the default 150 s re-announce
interval (`MeshNodeConfig::capability_reannounce_interval`, `mesh.rs:3698`) a
node applies ~6.7k announcements per second. Ed25519 verification dominates
that budget and is out of scope here, but the fold's share should be
allocation-free.

The existing `capability_fold_insert` bench (`benches/net.rs:2304`) cannot see
any of this. It builds `sample_capability_set` and a `CapabilityAnnouncement`
inside `b.iter`, so its ~31 µs per node is mostly fixture construction. There
is no apply-only bench, no refresh bench, no sweep bench, and no footprint
measurement.

### 4. The capability-set cache is invalidated by every announcement in the fleet

`CapabilitySetCache` (`capability_bridge.rs:807`) keys hits on the fold's
single change generation (`Fold::change_generation`, `mod.rs:331`), and
`Fold::apply` bumps that generation on every accepted Insert and Replace
(`mod.rs:386`, `mod.rs:442`), including the refresh case where
`index_payload_equivalent` already proved nothing indexed changed. At 6.7k
applies per second the cache is invalidated every ~150 µs, so the per-packet
greedy-admission path at `mesh.rs:32220` and the per-candidate placement
scorer effectively never hit. The multi-µs `synthesize_capability_set`
(`capability_bridge.rs:752`) that PERF_AUDIT_2026_06_10 §4.1 put the cache in
front of comes back at scale.

The cache's own doc comment says "announcement rates are low relative to
scoring/per-packet rates, so the cache stays warm in steady state." That is
true at hundreds of nodes and false at hundreds of thousands.

### 5. Two query-path leftovers

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
  is already a digest. `by_node` also holds a full `HashSet` per node for
  what the legacy path makes exactly one key (`capability_bridge.rs:748`).

## Operating point

The plan's finish line is **1M publishers at the default 150 s re-announce
interval, with the full fold held on server-class nodes.** That is ~6.7k
applies per second per receiving node. Every slice's proof is ultimately
read against that figure.

What the plan cannot move, even with every slice landed: every node that
holds the full fold still ingests every other node's announcement. At the
operating point and ~1.5 KB per announcement that is ~10 MB/s (~80 Mbit/s)
of announcement traffic into each such node, plus one Ed25519 verification
per announcement (out of scope; see below). Those costs are O(fleet) per
node by construction. Beyond the operating point, or for nodes that cannot
afford that ingest (browser leaves, mobile, constrained devices), the answer
is not more fold optimization but not holding the full fold: aggregators
hold it and leaves query them (`behavior/aggregator/`). A follow-up plan
that pushes past 1M starts there, not here.

## Design

Five independent tracks. Each is shippable on its own and each has a
measurement that would catch it being wrong. Order is by payoff per risk; the
dependency between tracks is only that Slice 0 must land first so every later
slice has a before number.

### Track A: expiry without the restart penalty

Two steps. A1 is cheap and fixes the mass-expiry shape on its own; A2 is
built only if Slice 0 shows the steady-state scan matters.

**A1: single-pass candidate collection.** Walk `entries` once under the read
lock and collect every key with `expires_at <= now` into a `Vec`, then evict
that list in `SWEEP_CHUNK_SIZE` slices, each under its own write-lock
acquisition, with the existing per-key re-check. The read-lock walk is
O(N), the write side O(E), so mass expiry drops from N·E/2048 visits to
N + E with no new structure and no new invariant to keep in sync. The
write-lock hold per chunk is unchanged. The one cost is a single read-lock
hold for the whole walk instead of ~100 shorter ones; at 1M entries that is
a few milliseconds, during which applies wait and queries proceed (they take
read locks too). If that hold measures as a problem, walk in fixed-size
stretches of the map and release the read lock between them. A resize
between stretches can make the walk skip or repeat entries: a repeat is
absorbed by the per-key re-check, and a skipped expired entry is caught on
the next tick, 500 ms later.

A1 does nothing for the steady state: the walk still runs twice a second
and still finds nothing. At ~1% of a core for 1M entries that is not worth
an invariant on its own, which is why A2 is conditional.

**A2: time-ordered expiry (conditional).** Keep a side structure ordered by `expires_at`, maintained wherever
`expires_at` is set or an entry is removed: `Fold::apply` (Insert and
Replace), `Fold::evict_node`, `Fold::restore`, and the sweep itself.

Recommended shape: a coarse timing wheel keyed by whole seconds,
`BTreeMap<u64 /* secs since fold epoch */, SmallVec<[K::Key; 4]>>`, living
inside `FoldState<K>` so it is covered by the existing `state` lock and the
existing `state → index` lock order. The sweep then pops every bucket at or
below `now`, re-checks `expires_at` per key exactly as today (a refresh may
have moved the entry to a later bucket without removing it from the old one;
the re-check already handles that case and the stale slot is simply dropped),
and never looks at a live entry. Cost is O(expired + stale slots).

Why not `BTreeMap<Instant, Key>` with exact instants: the map is as large as
`entries` and every apply pays a tree insert. Second-granularity buckets
collapse the ~1M entries of a 150 s re-announce cycle into ~150 buckets.
Why not a heap: a refresh cannot cheaply move an entry, and lazy deletion
leaves a heap the size of the refresh history, not the fleet.

Why not just lower `SWEEP_CHUNK_SIZE` or raise the interval: both change the
constant, neither changes the N·E shape. A1 changes the shape; A2 removes
the N.

The chunked write-lock discipline stays. Buckets are drained in
`SWEEP_CHUNK_SIZE` slices with the lock released between them, so the
sub-millisecond hold the current comment promises still holds.

### Track B: intern tags, then bitmap the buckets

Two steps, the first useful alone.

**B1: a fold-owned tag interner.** `TagId = u32`. The interner is a
`RwLock<(HashMap<Arc<str>, TagId>, Vec<Arc<str>>)>` held by the fold next to
`state` and `index` (lock order `state → index → interner`; the interner is a
leaf and is only taken on a miss). `CapabilityMembership::tags` stays
`Vec<String>` on the wire; the fold entry stores `Vec<TagId>` in a new
`IndexedMembership` that wraps the payload. `by_tag` and `by_synthetic`
become `HashMap<TagId, …>`, `by_region` likewise via the same interner,
and every `tag.clone()` on the index path disappears. Interning happens in
`translate_announcement` (outside the fold's locks), so the apply path is
handed ids and allocates nothing.

Decision to make, with a recommendation: whether `FoldEntry.payload` keeps the
`Vec<String>` beside the `Vec<TagId>`. Recommendation: no. Readers that need
strings (`capability_tags_for`, `tags_union_for`, the snapshot envelope,
`PreparedScope::matches`) resolve ids through the interner, which is one
`Arc<str>` clone per tag. Keeping both would halve the memory win for no
reader that cannot be served by resolve. This recommendation is conditional
on no reader needing the original signed bytes; see the corresponding risk.

Interning is bounded before it lands: a per-announcement cap on tag count
and length, and a per-publisher cap on distinct interned tags (see Risks).
Without them the interner is a memory sink that any admitted publisher can
fill.

**B2: roaring bitmaps over dense node slots.** Assign each `NodeId` a `u32`
slot on first sight (the `by_node` map already exists and is the natural
owner). With the legacy path producing exactly one `(class = 0, node)` key
per publisher, a bucket is a set of slots, and `roaring::RoaringBitmap` is
the right container: AND and OR are word-level, the footprint is a few bits
per membership for dense tags, and iteration comes out sorted, which deletes
the `sort_unstable` + `dedup` at `capability_bridge.rs:1384` and `:1614`.
`roaring` is not a dependency today; it is a pure-Rust crate with no build
step and is the standard choice. Multi-class keys (`class != 0`) are reserved
by the design but unused by any producer; the bitmap keys on slot and the
class is carried in the entry, with a `debug_assert` that keeps the one-key
invariant honest until a second producer appears.

Why B2 after B1 and not instead: B1 removes the 1.5 GB; B2 removes the
700 MB and the sort. B1 is a type change; B2 is a data-structure change with
a new dependency. If B2 is cut, B1 still earns its keep.

### Track C: allocation-free apply

Independent of B, and smaller:

- `on_insert`: probe `get_mut` first, fall back to `insert` on miss. Under B1
  this becomes moot for `by_tag` (the key is `Copy`), but `by_region` and
  the synthetic map need it until then, and it is a two-line change.
- `derive_synthetic_index_tags`: replace `Tag::parse` with borrowing prefix
  matches (`strip_prefix("software.model.")`, split at `.id=`, and so on)
  that return `&str` slices, and produce the synthetic key through the
  interner (B1) or a small-string `format!` into a reused buffer (pre-B1).
  `target_matches_filter_agrees_with_find_nodes_matching`
  (`capability_bridge.rs:2457`) already pins that the synthetic derivation
  and `translate_filter` stay inverses, so a rewrite that diverges fails a
  test rather than a query.
- `translate_announcement`: build the hardware summary and the tag list in
  one pass over `ann.capabilities.tags` instead of `views()` plus a
  `to_string` map; with B1 the output is `Vec<TagId>` and the
  `scope:region:` scan becomes an id compare.

### Track D: per-entry cache validity

Add a per-entry apply stamp to `FoldEntry` (a `u64` taken from the fold's
change counter at the moment the entry was written) and key
`CapabilitySetCache` hits on `(node_id, stamp)` instead of the global
generation. `get_or_synthesize` then reads the stamp under the same
`with_state` that `synthesize_capability_set_if_known` already takes, so
there is no second lock. A cached set survives every other node's
announcements and is invalidated only when its own publisher re-announces.

The ordering argument in the cache's current comment carries over: stamp
before synthesize, so a concurrent apply to the same node makes the entry
miss once rather than serve stale.

Decision, with a recommendation: whether `Fold::apply` should also skip
`signal_changed` when the old and new payloads are byte-equal, not merely
index-equivalent. Recommendation: no, not in this plan.
`subscribe_changes_fires_on_real_mutations_only` (`fold/tests.rs:173`) pins
that a Replace signals, the `watch_tools` consumer at `mesh.rs:34879`
debounces already, and D alone removes the cache cost. A watch-semantics
change belongs in its own plan with the watcher owners in the room.

### Track E: query leftovers

- `group_union` returns `CandidateKeys::Borrowed` for a single-tag group and
  materializes only for multi-tag groups. Under B2 a multi-tag union is a
  bitmap OR and this distinction disappears.
- Seed selection picks the smallest bucket across `tags_all`, every group,
  state, and region, then streams: iterate the seed, probe the others,
  collect into a `Vec`. No intermediate owned set. Under B2 this is an AND
  chain in bucket-size order.
- `FoldKind` gains an associated `type KeyHasher: BuildHasher + Default =
  RandomState`, and `CapabilityFold` sets it to `BuildU64TupleHasher`.
  `FoldState::entries` and `by_node` use it. Routing and reservation keep the
  default; their keys are not all digests.
- `by_node`'s value becomes `SmallVec<[K::Key; 1]>` with linear membership
  checks. `evict_node` and the single-target walk are unaffected in shape.

## The slices

### Slice 0: measure before touching anything

Delivers the benches and the footprint probe every later slice reports
against. In `benches/net.rs`:

- `capability_fold_apply/{insert,refresh_equivalent,replace_changed}`: signed
  envelopes built once outside `b.iter`, measuring `Fold::apply` alone at
  10k, 100k and 1M resident entries. The 1M point is the operating target
  and is where cache effects show: the existing query benches already lose
  ~45% of their per-node speed between 1k and 50k nodes.
- `capability_fold_sweep/{steady,mass_expiry}`: 100k and 1M entries, sweep
  with 0 expired and with 10% expired, measuring `sweep_expired_now`. The 1M
  `steady` number is what decides whether Slice 7 is built.
- `capability_fold_footprint`: not a Criterion bench. A `#[test] #[ignore]`
  under `fold/tests.rs` that builds 100k entries and reports bytes via a
  counting global allocator, printed per entry, run by hand with
  `cargo test --lib --features "$UNIT_FEATURES" -- --ignored fold_footprint --nocapture`.
  Interning's payoff depends entirely on how often tags repeat across
  nodes, so the probe runs twice: once with `sample_capability_set`'s
  repetition and once with a fixture where a fixed fraction of each node's
  tags is unique to it. Report both; a single number measured on the
  fixture's repetition would overstate B1 the way the old insert bench
  overstated apply.
- `capability_set_cache/hit_rate_under_announce_load`: 1k nodes scoring
  while a second thread applies refreshes at 1k/s; reports hits per
  `get_or_synthesize`.

Proof: the numbers land in this file's Status section with the command and
the machine. Slice 0 is done when the four tables exist.

### Slice 1: per-entry cache validity (Track D)

First after measurement because it has the best payoff per risk in the plan:
a `u64` stamp per entry and a different cache key, fixing a cost on the
per-packet admission path that already bites at ten thousand nodes (~67
invalidations per second at the 150 s default), long before fleet scale.

Delivers the entry stamp and the re-keyed cache.

Proof: `capability_set_cache/hit_rate_under_announce_load` goes from ~0 to
near 100%. Existing cache tests at `capability_bridge.rs:2867-3003` stay
green. New witness `cache_survives_other_publishers_announcements`: cache
node A, apply a refresh from node B, assert A hits; apply a refresh from A,
assert A misses once.

### Slice 2: single-pass expiry sweep (Track A1)

Delivers one read-locked walk that collects every expired key, then chunked
write-locked eviction with the existing re-check.

Proof: `capability_fold_sweep/mass_expiry` at 100k/10% drops by at least an
order of magnitude (from ~N·E/2048 visits to ~N + E); `steady` is unchanged,
by design. The three existing sweep tests at `fold/tests.rs:1248`, `:1315`,
`:1328` and `background_sweeper_evicts_expired_entries_on_tick` stay green.
New witness `mass_expiry_visits_each_entry_once`: 100k entries, 10% expired,
assert the sweep's entry-visit counter (exposed on `FoldMetrics` for this
purpose) is at most `entries + expired`.

### Slice 3: allocation-free apply (Track C)

Delivers the `get_mut`-first insert, the borrowing synthetic derivation, and
the single-pass translate.

Proof: `capability_fold_apply/insert` and `replace_changed` improve;
`refresh_equivalent` is unchanged (it never touched the index). The
agreement test at `capability_bridge.rs:2457` and the synthetic-tags-stay-
index-only test stay green. An allocation-counting assertion in the apply
bench's harness pins zero allocations on the `refresh_equivalent` path.

### Slice 4: tag interning (Track B1)

Delivers the interner, `Vec<TagId>` entries, id-keyed index maps, and
id-resolving readers.

Proof: `capability_fold_footprint` per-entry bytes drop by the predicted
factor (target: under 300 bytes per entry for the bench fixture, from ~2 KB);
the full fold unit surface plus `org_routing_wiring_tests` and the
`cross_lang_*` suites stay green, since the wire shape is untouched. Snapshot
round-trip (`snapshot_round_trips_via_restore`, `fold/tests.rs:307`) proves ids never leak into the
envelope.

### Slice 5: bitmap buckets (Track B2)

Delivers `roaring` buckets over node slots and deletes the result sort.

Proof: `capability_fold_scaling/query_complex/50000` closes to within 20% of
`query_tag/50000`; `query_tag_rare` stays flat; `find_nodes_matching` output
is byte-identical to the pre-slice output on the 10k fixture (a one-off
comparison test that keeps the old sort as the oracle for the slice's PR,
then is deleted). Footprint drops again.

### Slice 6: query leftovers (Track E)

Delivers borrowed single-tag groups, selectivity-ordered streaming
intersection, the `KeyHasher` associated type, and the `SmallVec` `by_node`.
Items made moot by Slice 5 are dropped from this slice when it is cut.

Proof: the existing `capability_fold_query` and `capability_fold_scaling`
benches; `find_nodes_matching_dedupes_publisher_across_classes`
(`capability_bridge.rs:2402`) for the `by_node` shape change.

### Slice 7: time-ordered expiry wheel (Track A2, conditional)

Built only if Slice 0's `capability_fold_sweep/steady` at 1M entries, or a
profile of a production-sized fold, shows the twice-a-second empty walk
costs more than the wheel's invariant is worth. Otherwise this slice is
dropped and the plan is complete at Slice 6.

Delivers the per-second wheel in `FoldState`, maintained by apply, evict,
restore, and the sweep.

Proof: `capability_fold_sweep/steady` drops from a full-map walk to
microseconds; `mass_expiry` holds Slice 2's number or better; the existing
sweep tests stay green. New witness
`sweep_cost_is_independent_of_live_entry_count`: two folds, 1k and 100k
live entries, same 100 expired, assert the entry-visit counter is equal.

## Risks

- **The wheel desynchronizes from `entries`** (Slice 7 only; A1 has no side
  structure to desynchronize, which is most of why it goes first). A code path sets or clears
  `expires_at` without touching the wheel, and an entry either never expires
  or is visited forever as a stale slot. Fallback: the sweep's per-key
  re-check already tolerates stale slots, and a `debug_assert` after every
  sweep that `wheel_len <= entries.len() + stale_slots_this_tick` catches
  the leak in tests. `restore` is the path most likely to be forgotten; its
  test rebuilds the wheel and asserts the first sweep after restore evicts
  exactly the entries whose TTL elapsed.
- **Interner growth is unbounded, and the publisher controls it.** This is
  a security risk, not only an operational one. Tags are publisher-chosen
  strings and the interner never forgets one, so a single admitted node that
  emits fresh unique tags in every announcement grows every receiver's
  memory permanently, for the life of the fold. The `FoldStats::interned_tags`
  gauge detects that; it does not prevent it. Slice 4 does not land without
  a stated bound, made of two parts: a per-announcement cap on tag count and
  tag byte length, enforced before interning (no capability-tag cap was
  found under `behavior/` when this was written; the only tag cap there is
  `MAX_METADATA_TAGS` in `metadata.rs`, which does not cover
  `CapabilityMembership`), and a per-publisher cap on distinct interned tags,
  past which that publisher's new tags are rejected or counted against it
  rather than interned. With both, interner size is bounded by
  `publishers × per-publisher cap` instead of by attacker patience. Ids are
  `u32`, so 4 billion distinct tags is the hard ceiling either way. A
  compaction pass is noted under Not in scope.
- **Dropping the payload strings breaks something that needs the signed
  bytes.** Track B1 recommends storing only `Vec<TagId>`. That is safe only
  if nothing re-serves, forwards, or re-verifies the original signed
  announcement from the fold entry's payload, for example a snapshot sync to
  a peer that checks the publisher's signature. This was not verified when
  the plan was written. Slice 4 starts by listing every reader of
  `FoldEntry.payload` and stating, per reader, whether it needs the original
  bytes; if any does, the entry keeps the signed envelope (or its bytes)
  and the memory target is revised. `snapshot_round_trips_via_restore`
  proves ids do not leak into the envelope; it does not prove signatures
  still verify after a round trip, and Slice 4 adds a test that does.
- **Dense node slots are never reclaimed.** A slot allocated for a node that
  leaves is held until the fold is restored. At the operating range that is
  4 bytes per departed node per bucket at worst (bitmaps are sparse where a
  slot is absent). Fallback: `evict_node` and expiry clear the slot from
  every bucket today already; a free-list for slot reuse is a follow-up if
  churn measurements show it matters.
- **The `roaring` dependency.** New crate in the core. It is pure Rust,
  widely used, and `no_std`-capable; the risk is review bandwidth, not
  build. Fallback: Slice 5 is cut and Slice 6 keeps the sort.
- **The stamp-keyed cache serves stale after `with_state_mut`.** The
  owner-projection retraction through `with_state_mut` at `mod.rs:691` mutates a payload field in
  place without going through apply. Fallback: `notify_projection_retracted`
  already bumps the change generation; Slice 1 makes it also bump the
  entry's stamp, and the retraction tests at `capability_bridge.rs:3786`
  extend to assert the cache misses afterwards.
- **A benchmark that measures the fixture again.** Slice 0 builds every
  envelope outside `b.iter` and asserts, in the bench itself, that a
  pre-built envelope applies in under a microsecond on an empty fold, so a
  later edit that moves construction back inside the loop is loud.

## Not in scope

- **Ed25519 verification cost per announcement.** It dominates the ingest
  budget at scale and is independent of the fold. Batch verification is its
  own plan.
- **Changing the wire form of `CapabilityMembership`.** Tags stay
  `Vec<String>` on the wire; interning is receiver-local. A compact tag
  codec is the deferred item in PERF_AUDIT_2026_05_28_CAPABILITY §"What was
  deferred".
- **Skipping the change signal on byte-equal refreshes.** A watcher-semantics
  change; see Track D's decision.
- **Interner compaction or slot reclamation.** Noted under Risks with the
  gauge that would justify them.
- **Merging the `state` and `index` locks, or replacing them with a
  snapshot structure.** Fewer lock operations per query, but the measured
  reads already parallelize (`capability_fold_concurrent`), and nothing in
  this plan changes the lock shape.
- **Routing and reservation folds.** Track A lands in the generic runtime
  and benefits them for free; Tracks B through E are capability-only.
- **The `DashMap`-backed graphs and the 40 ns capability check.** Both are
  design choices for the million-node target, not regressions.
