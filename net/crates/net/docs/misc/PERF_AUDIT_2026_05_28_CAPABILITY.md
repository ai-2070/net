# Perf Audit 2026-05-28 — Capability subsystem regressions

## Context

Comparing two M1 Max Criterion runs:

- Run 1: `benchmarks/BENCHMARK_RESULTS_M1_MAX.md` (518 benchmarks, pre-Phase-A.5.N)
- Run 2: `benchmarks/BENCHMARK_RESULTS_M1_MAX_2.md` (238 benchmarks, post-Phase-A.5.N)

Outside the capability subsystem the runs are essentially flat (median +0.49%; `auth_guard` and `ingestion` slightly improved). The signal is concentrated in `capability_filter`, `capability_set`, and `capability_announcement`.

## Headline regressions

| Suite | Benchmark | Run 1 | Run 2 | Δ |
|---|---|---|---|---|
| net | capability_filter/match_gpu_vendor | 3.74 ns | 46.17 µs | +1,235,038% |
| net | capability_filter/match_min_memory | 3.74 ns | 46.16 µs | +1,234,407% |
| net | capability_filter/match_complex | 10.28 ns | 47.04 µs | +457,493% |
| net | capability_set/has_model | 0.93 ns | 620.70 ns | +66,325% |
| net | capability_set/serialize | 930 ns | 43.97 µs | +4,627% |
| net | capability_announcement/serialize | 1.23 µs | 45.56 µs | +3,593% |
| net | capability_set/roundtrip | 2.69 µs | 50.59 µs | +1,778% |
| net | capability_filter/match_no_match | 3.12 ns | 63.46 ns | +1,937% |
| net | capability_set/deserialize | 1.72 µs | 6.60 µs | +283% |
| net | capability_announcement/create | 374.61 ns | 1.15 µs | +207% |

(`capability_index_*` was removed entirely; the new `capability_fold_*` family runs in the ms range but is not comparable to anything in run 1.)

## Root cause

Phase A.5.N moved `CapabilitySet` from typed fields (`HardwareCapabilities`, `Vec<ModelCapability>`, …) to a canonical `tags: HashSet<Tag>` storage. Typed projections are now decoded on demand via `caps.views()`. Three specific costs dominate:

### 1. `sorted_tag_vec` allocates a `String` per sort comparison

`net/crates/net/src/adapter/net/behavior/capability.rs:1784`

```rust
fn sorted_tag_vec(tags: &HashSet<Tag>) -> Vec<Tag> {
    let mut v: Vec<Tag> = tags.iter().cloned().collect();
    v.sort_by_key(|a| a.to_string());   // alloc per comparison
    v
}
```

For ~35 tags this is O(N log N) `String` allocations — most of the 46 µs floor.

### 2. `CapabilityFilter::matches` builds a fresh `CapabilityViews` per call

`net/crates/net/src/adapter/net/behavior/capability.rs:2369`

```rust
pub fn matches(&self, caps: &CapabilitySet) -> bool {
    // ... tag/model/tool checks ...
    let views = caps.views();   // fresh OnceCells per call
    if let Some(min_mem) = self.min_memory_gb {
        if views.hardware().memory_gb < min_mem { return false; }
    }
    if let Some(vendor) = self.gpu_vendor {
        if views.hardware().gpu_vendor() != Some(vendor) { return false; }
    }
    // ...
}
```

The `OnceCell`s on `CapabilityViews` only memoize within a single `matches()` call. Each benchmark iteration:

1. Sorts the tag set (cost #1)
2. Calls `hardware_from_tags()` — linear scan of every tag with `axis_key()` parse + `value.parse()` per match
3. (`match_complex` additionally forces `models_from_tags()`)

`has_gpu()` (line 1396) already shows the cheap pattern — a direct `tags.contains(&Tag::AxisPresent { ... })`. It's the only predicate that survives at ns scale; the others go through `views()`.

### 3. `has_model` / `has_tool` are linear scans with per-tag parsing

`net/crates/net/src/adapter/net/behavior/capability.rs:1349` and `:1372`. Previously a `Vec` field lookup; now scans every tag, calls `axis_key()`, checks axis prefix, splits on `.`, compares. ~666× slower.

### 4. `to_bytes` uses JSON over a larger tag set

`net/crates/net/src/adapter/net/behavior/capability.rs:1426`

```rust
pub fn to_bytes(&self) -> Vec<u8> {
    // Use JSON for now (can optimize to binary later)
    serde_json::to_vec(self).unwrap_or_default()
}
```

The "for now" arrived. The canonical tag set has roughly an order of magnitude more JSON tokens than the typed structs did.

## Proposed fixes, ranked

| # | Fix | Expected win | Risk | Touch |
|---|---|---|---|---|
| 1 | Replace `sort_by_key(\|a\| a.to_string())` with a derived/structural `Ord` on `Tag` and `sort_unstable()`. Lift the canonical sort key into `Tag` itself if needed. | 5-10× on every `views()` call. | Low — pure perf, no API change. | `behavior/tag.rs`, `capability.rs:1784` |
| 2 | Add tag-direct fast paths in `CapabilityFilter::matches`. `with_gpu_vendor` / `with_min_memory` / `with_min_vram` look up `hardware.gpu.vendor=…`, `hardware.memory_gb=…`, `hardware.gpu.vram_gb=…` directly from `caps.tags` the way `has_gpu()` already does. Avoid `views()` entirely for simple predicates. | 100-1000× on `match_gpu_vendor` / `match_min_memory` — back to ns range. | Low — `has_gpu` already established the pattern; complex predicates can still fall through to `views()`. | `capability.rs:2369` |
| 3 | Switch `CapabilitySet::to_bytes` / `from_bytes` (and the same on `CapabilityAnnouncement`) from `serde_json` to `bincode` or `postcard`. | 10-20× on serialize/deserialize/roundtrip, plus smaller wire size. | Medium — wire-format change. Needs version bump or feature flag; signed-announcement canonicalization has to be re-derived. | `capability.rs:1426`, `:2191`, `tag_codec.rs` (canonical ordering) |
| 4 | Cache projections on `CapabilitySet` itself: move the `OnceCell<HardwareCapabilities>` etc. onto the struct, invalidate on mutation. Repeated `views()` calls on the same set become a pointer load. | Helps any caller that calls `matches()` on the same set in a loop. | Medium — touches every mutator, has to handle `Clone` and serde (`#[serde(skip)]`). | `capability.rs` struct + mutator surface |
| 5 | Build a lazy `HashMap<String, TagIdx>` index for `software.model.<i>.id` / `software.tool.<i>.tool_id` lookups; back `has_model` / `has_tool` with it. | 10-50× on `has_model` / `has_tool`. | Medium — needs `OnceCell` + careful invalidation. | `capability.rs:1349`, `:1372` |

#1 and #2 are the cheap wins — together they likely reclaim ~80% of the regression with no API change. #3 is the biggest single win for serialize/roundtrip but is a wire-format decision. #4 and #5 compound on top.

## Caveats

- All numbers come from a single M1 Max snapshot. Re-run `cargo bench --bench net -- capability_` before and after any fix to confirm.
- The Phase A.5.N comment block (`capability.rs:1592`) explicitly anticipated this: the typed-struct fields were removed in favor of the tag-set source of truth. Fix #4 (caching projections on the set) partially reverts that direction — worth a deliberate decision before adopting.
- `capability_index_*` is gone; `capability_fold_*` is its replacement and operates in the ms range. That family is a separate perf workstream — not covered here.

## Source pointers

- Bench: `net/crates/net/benches/net.rs:2019` (`bench_capability_set`), `:2066` (`bench_capability_announcement`), `:2097` (`bench_capability_filter`)
- Code: `net/crates/net/src/adapter/net/behavior/capability.rs`
- Tag codec: `net/crates/net/src/adapter/net/behavior/tag_codec.rs`
- Parser used for comparison: `net/crates/net/benchmarks/parse_criterion.py`
