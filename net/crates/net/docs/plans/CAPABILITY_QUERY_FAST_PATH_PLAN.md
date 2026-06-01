# Capability fold query fast path

## Status

**Layers 0–2 landed; Layer 3 satisfied.** Performance rework of the `CapabilityFold` bulk query
path (`find_nodes_matching` → `Fold::query(Composite)` → `composite_query`). Layered into four
independently-shippable steps; the first earns its existence on its own. No behavior change —
every existing correctness test stays green. Touches
`src/adapter/net/behavior/fold/capability.rs`,
`src/adapter/net/behavior/fold/capability_bridge.rs`, and `mod.rs`.

Result over a 10k-node fold (before → after, all three layers):

| query | before | after | factor |
|---|---|---|---|
| `query_single_tag` | ~14 ms | ~0.18 ms | ~80× |
| `query_complex` | ~14 ms | ~0.37 ms | ~38× |
| `query_require_gpu` | ~29 ms | ~0.79 ms | ~37× |
| `query_gpu_vendor` | ~29.5 ms | ~1.0 ms | ~29× |
| `query_min_memory` | ~29.7 ms | ~0.87 ms | ~34× |
| `query_model` | ~108 ms | ~0.19 ms | ~560× |
| `query_tool` | ~108 ms | ~0.79 ms | ~137× |

Layer 3 (the plan's confidence step) is satisfied without new work: the existing `query_model` /
`query_tool` benches *are* the indexed-path regression guards, and Layer 1 added the
group-translation test, the bulk/single-target agreement test extended to model/tool/gpu/vendor,
and the synthetic-tags-stay-index-only test.

The original cost, for the record — a real cost, not a benchmark artifact. Over a 10k-node fold:

| query | time | why |
|---|---|---|
| `query_single_tag` (indexed, non-selective tag) | ~14 ms | clones the whole bucket + every payload |
| `query_require_gpu` / `query_min_memory` | ~29 ms | no index seed → full scan + clone |
| `query_model` / `query_tool` | ~108 ms | full scan + clone + **re-parse every tag of every candidate** |

The locking is already correct — `Fold::query` read-acquires both the state and index locks
(`mod.rs:433-437`), so concurrent queries genuinely parallelize. The `capability_fold_concurrent`
benchmark's 42-second number is pure volume (2,000 queries × ~21 ms each), not contention; per-op
latency under 4 threads is *lower* than the single-thread average, confirming reads scale. The fix
is to make each individual query cheaper, not to touch the locks.

## The gap

The bulk query path is `find_nodes_matching` (`capability_bridge.rs:407`), which calls
`fold.query(CapabilityQuery::Composite(...))`, gets back `Vec<CapabilityMatch>` (where
`CapabilityMatch = ((u64, NodeId), CapabilityMembership)`), then post-filters and keeps only the
`NodeId`s. Five distinct cost drivers, in rough order of damage:

1. **Whole-candidate-set deep clone before any post-filter.** `composite_query`
   (`capability.rs:433-436`) does `e.payload.clone()` for *every* candidate. A
   `CapabilityMembership` is not cheap to clone — a `Vec<String>` of tags, a `BTreeMap<String,
   String>` of metadata, and three allow-list `Vec`s. `find_nodes_matching` then immediately
   discards the payload (`capability_bridge.rs:411-414`) and keeps only the `NodeId`. For the
   108 ms cases we clone 10,000 full payloads to extract 10,000 `u64`s and throw the rest away.

2. **Non-indexed axes get no selective seed.** `require_gpu` / `min_memory` / `min_vram` /
   `gpu_vendor` / `require_models` / `require_tools` are not in the index at all. The index
   (`CapabilityIndexInner`, `capability.rs:188-196`) covers only `by_tag`, `by_region`, `by_state`.
   When a filter carries none of those — e.g. a bare `require_tool` — `composite_query` falls to the
   `else` branch (`capability.rs:388-391`) and seeds candidates = **every key**. The index buys
   nothing; the query is a full materialization followed by an in-memory post-filter.

3. **Per-candidate re-parse for model/tool filters.** `membership_passes_post_filter`
   (`capability_bridge.rs:128-134`) re-parses *every tag string* of *every candidate* into a `Tag`
   and builds a fresh `CapabilitySet` per candidate, only to call `has_model` / `has_tool`. This is
   allocation-heavy string parsing across the full 10k set and is what makes `query_tool` /
   `query_model` ~8× slower than the indexed tag path.

4. **Non-selective indexed tags clone the bucket twice.** For `require_tag("inference")` where
   nearly every node carries the tag, `resolve_keys_all_tags` clones the whole bucket into a fresh
   `HashSet` (`capability.rs:347`), then `composite_query` clones every matching payload on top.

5. **Dedup via a second `HashSet<NodeId>` + sort.** `find_nodes_matching`
   (`capability_bridge.rs:410-420`) inserts every match into a `HashSet<NodeId>`, collects to a
   `Vec`, then sorts. The HashSet is redundant allocation given the result is sorted anyway.

The single-target path already does the right thing: `target_matches_filter`
(`capability_bridge.rs:434`) walks the publisher's `by_node` keys, runs the tag intersection and
post-filter against a **borrowed** `&entry.payload`, and clones nothing. The bulk path just never
got the same treatment.

## Design

Four layers, ordered by impact-per-risk. Each is independently shippable; Layer 0 helps even if
the rest is deferred, and it establishes the borrowed-predicate shape the later layers build on.

### Layer 0 — Borrow-and-filter: never clone a payload the caller discards

The bulk caller needs `Vec<NodeId>`, not cloned payloads. Add a key-returning query path that runs
the post-filter against a borrowed `&CapabilityMembership` inside the read lock and returns only the
matched keys.

- Add an internal resolution helper (either a new `CapabilityQuery::CompositeKeys(CapabilityFilter)`
  variant, or a `Fold` method that runs a borrowed predicate over resolved candidates) that performs
  candidate resolution exactly as `composite_query` does today, but instead of
  `(*k, e.payload.clone())` it runs a `FnMut(&CapabilityMembership) -> bool` predicate and collects
  the surviving `(class, node)` keys.
- Move `membership_passes_post_filter` *into* that pass so the hardware / model / tool checks run on
  `&entry.payload`. No payload is ever cloned; everything happens under a single read-lock
  acquisition.
- Rewrite `find_nodes_matching` to: resolve keys (with the post-filter applied inline) → collect
  `node_id`s → sort → dedup. This is structurally identical to `target_matches_filter`, just over the
  full candidate set instead of one publisher's keys.

Risk: low. Pure internal refactor. The existing tests
(`find_nodes_matching_dedupes_publisher_across_classes`, the `membership_passes_post_filter_*` tests)
pin the observable behavior. Expected to eliminate cost drivers 1 and 3's clone/alloc entirely; both
the 14 ms and 108 ms cases drop substantially (the 108 ms case still scans all 10k keys until
Layer 1, but stops cloning payloads and stops re-parsing into owned `CapabilitySet`s).

### Layer 1 — Index the model / tool / gpu axes so they seed instead of full-scan

The key realization: models and tools are *already* in the tags, as canonical bundles
(`software.model.<i>.id=<name>`, `software.tool.<i>.tool_id=<name>` — see
`capability.rs:84-86`). Parse them **once at insert time**, not once per candidate per query.

- In `CapabilityIndexInner::on_insert` / `on_remove` (`capability.rs:199-235`), derive synthetic
  index keys `model:<name>` and `tool:<name>` from the publisher's bundle tags and add them to
  `by_tag`. These live **only** in the index — they are never written into the stored
  `payload.tags`, so `capability_tags_for` / `capability_tags_for_all` / `tags_union_for`
  (`capability_bridge.rs:453-491`) are unaffected. Add `by_gpu_vendor: HashMap<String,
  HashSet<key>>` and a `gpu_keys: HashSet<key>` populated from `payload.hardware` at insert.
- In `translate_filter` (`capability_bridge.rs:56`), push `require_models` / `require_tools` into
  `tags_all` as `model:<name>` / `tool:<name>`. They now flow through the existing selective
  `resolve_keys_all_tags` seed, and the per-query parse in `membership_passes_post_filter` is
  deleted outright. Add `require_gpu` / `gpu_vendor` as index-seeded axes in `composite_query`'s seed
  selection.
- Memory and VRAM are range predicates, so they can't seed from an exact-match bucket. Leave them as
  a borrowed post-filter (cheap after Layer 0) — but note that with Layer 1 they now run against a
  *seeded* candidate set whenever any other axis is present in the filter. A `BTreeMap<u32,
  HashSet<key>>` for range-seeding is possible if a memory-only query ever proves hot, but it is not
  in scope here.

This **contradicts the existing comment** at `capability_bridge.rs:57-62`, which says models/tools
are deliberately *not* pushed into `tags_all`. That reasoning is about matching against
*publisher-emitted* tags — an honest publisher never emits a bare `model:llama3`, so a `model:<name>`
entry in `tags_all` would never match real wire data. Using `model:<name>` / `tool:<name>` as
*insert-derived synthetic index keys* is a different mechanism: the index entry is manufactured by
`on_insert` from the bundle, not received on the wire. Both the comment and the
`translate_filter_passes_require_tags_through_and_defers_models_and_tools` test
(`capability_bridge.rs:621`) must be updated to reflect the new design and explain the distinction so
the next reader doesn't "fix" it back.

Risk: medium — insert-time parsing and a documented semantics change. Expected to move `query_tool` /
`query_model` from a full-scan into the indexed-tag ballpark, then faster once the synthetic tag is
selective.

### Layer 2 — Lazy candidate resolution, no intermediate `HashSet`

- `resolve_keys_all_tags` (`capability.rs:324`): for the single-tag common case, iterate the bucket
  by reference instead of cloning it into a fresh `HashSet` (`capability.rs:347`). For the multi-tag
  case, keep starting from the smallest bucket but filter by borrowing rather than materializing.
- In `find_nodes_matching`, collect `node_id`s into a `Vec`, then `sort_unstable` + `dedup` instead
  of the `HashSet<NodeId>` insert-then-collect-then-sort (`capability_bridge.rs:410-420`).

Risk: low. Trims the allocation profile on every query; most visible on the non-selective tag case
(driver 4).

### Layer 3 — Lock in the win with benches and tests

- Keep `capability_filter` and `capability_fold_query` benches as the before/after; the `query_tool`
  / `query_model` rows are the headline Layer 1 delta. Add a bench for the indexed model/tool path so
  the improvement is regression-guarded.
- Existing correctness tests must stay green; they pin the invariants the rework must preserve:
  cross-class dedup (`find_nodes_matching_dedupes_publisher_across_classes`), deterministic sort
  order, the `tags_all`-empty-matches-everything vs `tags_any`-empty-matches-nothing asymmetry
  (`capability.rs:140-147`), and `require_gpu` failing closed on missing hardware
  (`membership_passes_post_filter_enforces_min_memory_and_gpu`).
- Add a test asserting synthetic `model:` / `tool:` index keys never leak into `payload.tags` or any
  `capability_tags_for*` output.

## Implementation notes

- The candidate-resolution logic in `composite_query` (`capability.rs:363-441`) is the shared core.
  The cleanest shape is to split it into "resolve candidate keys" (returns the seed set / iterator)
  and "materialize," then have the existing `Vec<CapabilityMatch>` path and the new keys-only path
  both call the resolver. This avoids duplicating the seed-selection logic.
- Preserve the seed-selection priority order in `composite_query` (`tags_all` → `state` → `region` →
  `class` → full scan). Layer 1 inserts the new gpu/vendor axes into this ladder; place them after
  tags but before the full-scan fallback, since a tag is typically more selective than "has a GPU."
- The synthetic-tag derivation in `on_insert` must be the exact inverse of what
  `membership_passes_post_filter` currently parses, or model/tool queries will silently start missing
  nodes. Factor the bundle→name extraction into one function used by both the index builder and (if
  any caller still needs the post-filter form) the matcher, so they can't drift.
- `on_remove` must remove exactly the synthetic keys `on_insert` added; the existing
  empty-bucket-cleanup pattern (`capability.rs:212-234`) should be reused verbatim for the new maps so
  the index doesn't accumulate empty buckets.
- `CapabilityFilter`, `CapabilityQuery`, and `CapabilityMatch` are `pub`/`pub(crate)` within the fold
  module; adding a `CompositeKeys` variant is non-breaking for external callers because they
  construct filters, not query enums directly. Confirm no external match on `CapabilityQuery` needs a
  new arm before choosing the enum-variant approach over a private `Fold` method.

## Sequencing & risk

| Layer | Effort | Risk | Payoff |
|---|---|---|---|
| 0 Borrow-and-filter | ~half day | low — internal, tests pin behavior | large (removes all bulk-path clones) |
| 1 Index model/tool/gpu | ~1 day | medium — insert-time parsing, documented semantics change | large (kills the 108 ms full-scan) |
| 2 Lazy resolution | ~half day | low | moderate |
| 3 Benches/tests | ~half day | none | confidence |

Do 0 → 1 → 2 → 3. Layer 0 is the highest-confidence win and ships on its own; Layer 1 depends on
Layer 0's borrowed-predicate shape to be clean.
