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

## Results — fix #1 + #2 applied (2026-05-28)

Baseline taken on Windows host (different hardware than the M1 Max snapshot above — compare same-run before/after, not absolute ns vs M1 Max). Criterion baseline name: `pre-fix`. Both fixes landed together; columns are pre-fix vs post-fix on the same hardware.

| Benchmark | Pre-fix | Post-fix | Δ |
|---|---|---|---|
| `capability_filter/match_gpu_vendor` | 67.96 µs | **115.12 ns** | **−99.83%** (~590× faster) |
| `capability_filter/match_min_memory` | 58.94 µs | **25.75 ns** | **−99.96%** (~2289× faster) |
| `capability_filter/match_complex` | 59.85 µs | **4.41 µs** | **−92.65%** (~13.6× faster) |
| `capability_filter/match_require_gpu` | 74.90 ns | **38.91 ns** | **−47.98%** |
| `capability_filter/match_no_match` | 57.11 ns | **54.11 ns** | −5.4% |
| `capability_filter/match_single_tag` | 41.10 ns | 49.89 ns | **+21.2%** (small regression — see below) |

**`match_single_tag` regression (~9 ns absolute):** the rewritten `matches()` body has more `if let Some(...)` arms even for filters that only set `require_tags`, plus the `use` block at the top. The branches all predict cleanly but the added instruction count costs a few ns. Acceptable trade for the µs-scale wins on the targeted predicates; can be reclaimed later by hoisting the simple-tag fast path or splitting `matches()` into a fast / slow path on filter shape.

**Tests:** all 195 lib tests with "capability" in the name pass (`cargo test --features net --lib capability`). Wire format is unchanged (the wire serializer keeps `sorted_tag_vec` with `Tag::to_string()` order; only the decoder paths switched to derived `Ord`).

### What each fix did

- **Fix #1** (`capability.rs:1784`): added `decoder_sorted_tag_vec` using `Tag`'s derived `Ord` via `sort_unstable()`. Kept original `sorted_tag_vec` (the `Tag::to_string()` variant) in place for `serialize_tags_sorted` so signed-announcement bytes stay byte-stable across versions. Switched `CapabilityViews::sorted_tags()` and the three `From<&CapabilitySet>` impls (`HardwareCapabilities` / `SoftwareCapabilities` / `ResourceLimits`) to the new helper. Accounts for the bulk of the `match_complex` win since that bench still decodes the models projection.
- **Fix #2** (`capability.rs:2369`): added `CapabilitySet::axis_value(axis, key) -> Option<&str>` (pub(crate), `capability.rs:1404`). Rewrote `matches()` so single-field hardware predicates probe the tag set directly:
  - `min_memory_gb` → `axis_value(Hardware, "memory_gb")`
  - `gpu_vendor` → O(1) `HashSet::contains(&Tag::AxisValue { ... })` constructed from `gpu_vendor_str(vendor)` (made `pub(crate)` in `tag_codec.rs:168`)
  - `min_vram_gb` → `axis_value(Hardware, "gpu.vram_gb")` with fall-through to `views().hardware().total_vram_gb()` for multi-GPU configs
  - `min_context_length` / `require_modalities` still go through `views()` — they decode the models projection. The block is now lazily guarded so filters that don't set those fields never call `views()`.

### What's NOT addressed yet

| Family | Status |
|---|---|
| `capability_set/has_model` (~760 ns), `has_tool` (~740 ns) | **Fixed** — see "Results — fix #5" below |
| `capability_set/serialize` (~52 µs), `to_bytes`/`from_bytes` | Unchanged — needs fix #3 (bincode/postcard) |
| `capability_announcement/*` | Not re-benched (same JSON path; will move with fix #3) |
| Repeated `matches()` on same `CapabilitySet` in a loop | Each call still allocates a new `CapabilityViews` if it falls through to the modality/ctx path. Fix #4 (cache projections on the set) addresses this. |

## Results — fix #5 applied (2026-05-28)

Baseline `pre-fix-5` taken on the same Windows host after fixes #1 + #2 were already in. Both `has_*` benches scan the canonical tag set on every call; nothing in fixes #1 / #2 touched their hot path, so this is a clean before/after for #5 itself.

| Benchmark | Pre-fix-5 | Post-fix-5 | Δ |
|---|---|---|---|
| `capability_set/has_model` | 755.54 ns | **31.65 ns** | **−95.8%** (~24× faster) |
| `capability_set/has_tool`  | 680.02 ns | **34.69 ns** | **−94.9%** (~19.6× faster) |

### Root cause (not what the doc originally said)

The first diagnosis assumed the cost was the O(N) tag scan itself, and proposed a lazy lookup index. The actual cost turned out to be **`Tag::axis_key()`'s per-tag `String::clone`** (`tag.rs:299`):

```rust
pub fn axis_key(&self) -> Option<TagKey> {
    match self {
        Self::AxisPresent { axis, key } | Self::AxisValue { axis, key, .. } => {
            Some(TagKey::new(*axis, key.clone()))   // alloc per tag
        }
        ...
    }
}
```

The old `has_model` / `has_tool` (`capability.rs:1349`, `:1372`) called `axis_key()` for every tag in the set — ~35 String allocations per call.

### What fix #5 actually did

Direct `Tag::AxisValue` pattern match, no `axis_key()`, value compare first:

```rust
fn has_indexed_software_value(
    &self, family_prefix: &str, sub_key: &str, expected_value: &str,
) -> bool {
    self.tags.iter().any(|tag| match tag {
        Tag::AxisValue { axis: TaxonomyAxis::Software, key, value, .. }
            if value == expected_value =>
        {
            // Only the value-matching candidate(s) pay the key parse.
            let Some(rest) = key.strip_prefix(family_prefix) else { return false; };
            let Some((_idx, sub)) = rest.split_once('.') else { return false; };
            sub == sub_key
        }
        _ => false,
    })
}
```

`has_model` / `has_tool` are now thin wrappers around `has_indexed_software_value`. No caching, no `OnceCell`, no API change — just dropping per-tag allocations and reordering checks so the cheap one filters first.

**Implication for `axis_key()` itself**: any other caller that iterates a tag set through `axis_key()` is paying the same per-tag allocation. Worth a follow-up audit (grep for `axis_key()` callers) — likely candidates: predicate evaluation, `required_capability`, the hardware/software/models decoders. A non-cloning variant (`axis_key_ref(&self) -> Option<(TaxonomyAxis, &str)>`) would let those callers borrow.

## Results — `axis_key_ref` follow-up applied (2026-05-28)

The `axis_key()` audit above turned up 14 hot-path callers. Added a borrowing variant `Tag::axis_key_ref() -> Option<(TaxonomyAxis, &str)>` (`tag.rs:308`) and migrated:

- All 5 view decoders in `tag_codec.rs`: `hardware_from_tags`, `software_from_tags`, `resource_limits_from_tags`, `models_from_tags`, `tools_from_tags`
- All 5 `is_*_owned_tag` predicates in `tag_codec.rs`
- `Predicate::Exists` (`predicate.rs:1242`) and `match_axis_tag` helper (`predicate.rs:1904`)
- `RequiredCapability::AxisKey` (`required_capability.rs:71`)
- `MatchKey::{Axis, AxisKey}` (`capability_aggregation.rs:284`)

`axis_key()` kept for callers that need an owned `TagKey` (e.g. `diff.rs:551` which collects into `HashSet<TagKey>`).

Baseline `pre-axis-key-ref` taken on the same Windows host with fixes #1 + #2 + #5 already applied.

| Benchmark | Pre | Post | Δ |
|---|---|---|---|
| `capability_filter/match_complex` | 4.42 µs | **3.74 µs** | **−15.9%** (~683 ns) |
| `capability_set/has_tag` | 38.04 ns | 44.87 ns | +16.5% (~7 ns; rebuild noise — `has_tag` doesn't use `axis_key`, uses `Tag::parse` + `semantic_eq`) |

`match_complex` is the only filter bench that still falls through to `views().models()` (for the modality check); the saved 683 ns is the per-tag-allocation cost in `models_from_tags`'s axis_key iteration. Other callers (predicate eval, capability_aggregation, owned-tag predicates) get the same per-tag saving wherever they execute — not separately benched here.

All 4133 lib tests pass.
