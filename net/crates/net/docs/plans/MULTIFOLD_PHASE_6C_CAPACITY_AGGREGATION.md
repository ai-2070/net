# Multifold Phase 6c — Capacity Aggregation Plan

Aggregation surface on top of `Fold<CapabilityFold>` that lets
operators ask **"what's available with what kind of capacity, within X ms
latency"** — and answers it as a ranked materialized view across any tag
axis they care about (model, GPU, software, region, tool, custom).

The Rust framework lands the core types + methods; each binding
(Rust SDK, TypeScript, Python, Go, C) wraps the surface in its
idiomatic shape. Lives alongside the existing
`MULTIFOLD_PHASE_3B_CUTOVER.md` plan; this is purely additive on the
framework branch and doesn't depend on the cutover phases.

---

## Goals

1. **Generic aggregation primitive** — one method (`Fold::aggregate`)
   that composes a `TagMatcher × GroupBy × Aggregation` triple into a
   bucketed result. Covers "how many of X are on the mesh" for ANY X.
2. **Capacity ranking** — higher-level method
   (`Fold::capacity_ranking`) that wraps `aggregate` with RTT
   filtering + state breakdown + summed numeric capacity + sort +
   limit. The "give me the model / GPU / tag with the most available
   capacity within X ms latency" use case.
3. **Cross-binding parity** — the API is reachable from Rust SDK,
   TypeScript SDK, Python SDK, Go bindings, and the C ABI. Each
   binding ships its own wrapper + tests; the Rust core ships once.
4. **No new framework abstractions** — the aggregation lives on
   `impl Fold<CapabilityFold>` in `behavior/fold/capability.rs`.
   Operators reach it via the existing handle they already use for
   `query` / `apply` / `snapshot`.

---

## Non-goals

- **Cross-fold joins beyond RTT lookup.** The aggregation can filter
  by latency via an injected closure but doesn't reach into
  `RoutingFold` directly. Post-Phase-4b, a separate convenience
  helper can compose the two; until then operators wire the rtt
  closure to `proximity::ProximityGraph` or
  `Fold<RoutingFold>::query`.
- **Persistent materialized views.** The output of
  `capacity_ranking` is computed on demand from the live fold state.
  A future "cache the result for N ms" wrapper is fine but not in
  scope here.
- **Strong consistency across publishers.** Folds are eventually
  consistent. The aggregation reflects whatever the local fold knows
  at call time; concurrent publishes may land between two calls and
  yield different bucket counts.

---

## Core API (Rust framework)

### Filter shape: `TagMatcher`

```rust
pub enum TagMatcher {
    /// Exact tag string match: "software.python=3.11"
    Exact(String),
    /// Tag-string prefix: "hardware.gpu" matches
    /// "hardware.gpu" + "hardware.gpu.vram_gb=80" + ...
    Prefix(String),
    /// Tag is in the given taxonomy axis (any key).
    Axis(TaxonomyAxis),
    /// Tag has a specific (axis, key) regardless of value:
    /// matches "hardware.gpu.count=8" with
    /// AxisKey { axis: Hardware, key: "gpu.count" }
    AxisKey {
        axis: TaxonomyAxis,
        key: String,
    },
    /// Phase 6c-C: regex match against canonical tag form.
    Regex(String),
    /// Phase 6c-C: semver range against a specific axis-key value.
    /// "software.python=*" with min=3.10, max=3.13 picks python
    /// 3.10/3.11/3.12 entries.
    VersionRange {
        axis_key: String,
        min: Option<String>,
        max: Option<String>,
    },
}
```

Phase 6c-A ships the first four variants. Phase 6c-C adds `Regex`
(requires `regex` crate; ~200KB compile-time, ~few MB binary
overhead) and `VersionRange` (requires either `semver` crate or a
homebrew dotted-decimal compare — recommend `semver`).

### Group shape: `GroupBy`

```rust
pub enum GroupBy {
    /// Each entry's `class_hash`.
    Class,
    /// Each entry's `state` (Idle / Busy / Reserved / Faulty).
    State,
    /// Each entry's `region` (or "(none)" for unset).
    Region,
    /// Each entry's publisher `node_id` (distinct counts).
    Publisher,
    /// Bucket by tag stem — every tag matching the prefix becomes
    /// its own bucket. "model" with payload tags
    /// ["model.llama-3", "model.gpt-4"] produces buckets
    /// "llama-3" and "gpt-4".
    TagStem(String),
    /// Bucket by the value of a specific axis-key. "software.python"
    /// produces buckets "3.10", "3.11", "3.12".
    TagValue {
        axis: TaxonomyAxis,
        key: String,
    },
}
```

### Aggregation shape

```rust
pub enum Aggregation {
    /// Count of entries in each bucket.
    Count,
    /// Distinct publisher count (deduplicates on node_id).
    DistinctPublishers,
    /// Distinct values for a given (axis, key) within each bucket.
    /// Useful for "for each region, how many distinct GPU vendors?"
    DistinctValues { axis: TaxonomyAxis, key: String },
    /// Phase 6c-C: sum the numeric value of <axis_key>=<n> tags.
    /// "hardware.gpu.count" across matching entries → total GPU
    /// count on the mesh.
    SumNumericTag(String),
    /// Phase 6c-C: min/max of a numeric tag value across the bucket.
    MinNumericTag(String),
    MaxNumericTag(String),
}
```

### Base method

```rust
impl Fold<CapabilityFold> {
    /// Compose a matcher + group_by + aggregation into a bucketed
    /// result. Returned vector is sorted by bucket key (lex).
    pub fn aggregate(
        &self,
        matcher: Option<TagMatcher>,
        group_by: GroupBy,
        agg: Aggregation,
    ) -> Vec<(String, u64)>;
}
```

### Capacity ranking method

```rust
pub struct CapacityQuery {
    /// Optional pre-filter on entries before grouping.
    pub matcher: Option<TagMatcher>,
    /// How to bucket the matching entries.
    pub group_by: GroupBy,
    /// Drop entries whose publisher's RTT exceeds this. `None` = no
    /// RTT filter (consider every reachable entry).
    pub max_rtt_ms: Option<u32>,
    /// Optional numeric-tag axis to sum within each bucket
    /// (e.g. "hardware.gpu.count" for total GPU capacity).
    /// `None` skips the summed_capacity column.
    pub sum_axis_key: Option<String>,
    /// Top-N buckets by `available_count` (descending). 0 = all.
    pub limit: usize,
}

pub struct CapacityRow {
    /// Bucket key (the stem / value / state-name / region).
    pub bucket: String,
    /// Entries in state=Idle that pass the latency + matcher filters.
    pub idle: u64,
    /// Entries in state=Busy that pass.
    pub busy: u64,
    /// Entries in state=Reserved that pass.
    pub reserved: u64,
    /// Total reachable: idle + busy + reserved (faulty entries
    /// excluded — they're not really available).
    pub available: u64,
    /// Sum of the `sum_axis_key` numeric tag value across the
    /// bucket's matching entries. `None` if no `sum_axis_key` was
    /// requested or no entries carry the tag.
    pub summed_capacity: Option<u64>,
}

impl Fold<CapabilityFold> {
    /// Capacity-ranked materialized view over the fold.
    ///
    /// `rtt_lookup` is a caller-supplied closure that maps publisher
    /// `node_id` to current RTT in milliseconds. Pre-Phase-4b
    /// callers typically wire this to `ProximityGraph::rtt_ms_to`;
    /// post-Phase-4b they wire to `Fold<RoutingFold>::query`. The
    /// closure may return `None` for unknown nodes — the
    /// aggregation treats unknown RTT as "fails the filter" rather
    /// than as zero, so adding a slow filter doesn't surface
    /// never-pinged nodes as "fastest available."
    pub fn capacity_ranking<R>(
        &self,
        query: CapacityQuery,
        rtt_lookup: R,
    ) -> Vec<CapacityRow>
    where
        R: Fn(NodeId) -> Option<u32>;
}
```

### Operator query example

```rust
// "Top 5 GPU types available with latency ≤ 50 ms"
let view = fold.capacity_ranking(
    CapacityQuery {
        matcher: Some(TagMatcher::Prefix("hardware.gpu".into())),
        group_by: GroupBy::TagStem("hardware.gpu".into()),
        max_rtt_ms: Some(50),
        sum_axis_key: Some("hardware.gpu.count".into()),
        limit: 5,
    },
    |node_id| proximity_graph.rtt_ms_to(node_id),
);

for row in view {
    println!(
        "{:>20}  {} idle / {} busy / {} total  ({} GPUs)",
        row.bucket, row.idle, row.busy, row.available,
        row.summed_capacity.unwrap_or(0),
    );
}
// Sample output:
//                  h100  12 idle / 47 busy / 59 total  (472 GPUs)
//                  a100  8 idle / 24 busy / 32 total  (256 GPUs)
//                  l40s  4 idle / 12 busy / 16 total  (64 GPUs)
```

---

## Phasing

| Sub-step | Scope | LOC | Sessions |
|---|---|---|---|
| **6c-A** | `TagMatcher::{Exact, Prefix, Axis, AxisKey}` + `GroupBy::{Class, State, Region, Publisher, TagStem, TagValue}` + `Aggregation::{Count, DistinctPublishers, DistinctValues}` + `Fold::aggregate` + tests. NO RTT, NO regex/version, NO summed_capacity. | ~150 | 1 |
| **6c-B** | `CapacityQuery` + `CapacityRow` + `Fold::capacity_ranking` (uses 6c-A internally) + `sum_axis_key` + `Aggregation::SumNumericTag` + RTT closure + tests with stub rtt_lookup. | ~150 | 1 |
| **6c-C** | `TagMatcher::{Regex, VersionRange}` + `Aggregation::{MinNumericTag, MaxNumericTag}` + `regex` and `semver` deps + tests. | ~120 | 1 |
| **6c-D** | Rust SDK re-exports + doctests. | ~80 | 0.5 |
| **6c-E** | Node bindings + sdk-ts wrappers + vitest. | ~250 | 1-2 |
| **6c-F** | Python bindings + sdk-py wrappers + pytest. | ~200 | 1 |
| **6c-G** | Go bindings + go tests. | ~250 | 1-2 |
| **6c-H** | C SDK header + C ABI surface. | ~150 | 1 |

**Total: 7-10 sessions.** Sub-steps 6c-A through 6c-C land on the
multifold branch; 6c-D through 6c-H each land as their own
binding-side PRs once the Rust core is merged. The bindings can
ship in any order — they don't depend on each other.

---

## Binding work

### Rust SDK (`net/crates/net/sdk`)

**Surface:**
- Re-export `TagMatcher`, `GroupBy`, `Aggregation`, `CapacityQuery`,
  `CapacityRow` from
  `net_sdk::capabilities::aggregation::*`.
- Re-export `Fold::aggregate` and `Fold::capacity_ranking` via the
  existing SDK `Fold` handle (the SDK already wraps `Arc<Fold<K>>`
  in a thin Send + Sync newtype).
- Doctests on the SDK's public docs page showing the
  "top 5 GPU types" example.

**Tests:**
- Carry the framework tests forward through the SDK's public
  surface — same assertions, SDK-shaped types.
- New: `mesh.capability_capacity_ranking(query, |id| rtt_for(id))`
  end-to-end against an `Arc<Fold<CapabilityFold>>` registered on
  a real test MeshNode.

**Files:**
- `net/crates/net/sdk/src/capabilities/aggregation.rs` (new)
- `net/crates/net/sdk/src/lib.rs` (re-export)

### Node bindings (`net/crates/net/bindings/node` + `sdk-ts`)

**Wire shape:**
- `TagMatcher` / `GroupBy` / `Aggregation` cross the FFI boundary
  as JSON-encoded tagged unions. napi has good string round-trip
  performance and `serde_json` is already a dep.
- `rtt_lookup` is a JS function passed as a napi
  `ThreadsafeFunction<u64, ErrorStrategy::Fatal, u32>`. napi
  marshals each call back to JS on the event loop; the aggregation
  awaits each call. NOTE: this serializes RTT lookups; the Rust core's
  closure type is synchronous so the napi shim blocks on each call.
  For a typical query with ≤ 1000 candidate nodes this is fine.
- Results return as a `Vec<CapacityRow>` serialized to a JS array of
  plain objects.

**TypeScript surface (`sdk-ts/src/fold/capabilities.ts`):**

```typescript
export type TagMatcher =
  | { kind: "exact"; value: string }
  | { kind: "prefix"; value: string }
  | { kind: "axis"; axis: TaxonomyAxis }
  | { kind: "axis_key"; axis: TaxonomyAxis; key: string }
  | { kind: "regex"; pattern: string }
  | { kind: "version_range"; axis_key: string; min?: string; max?: string };

export type GroupBy =
  | { kind: "class" }
  | { kind: "state" }
  | { kind: "region" }
  | { kind: "publisher" }
  | { kind: "tag_stem"; prefix: string }
  | { kind: "tag_value"; axis: TaxonomyAxis; key: string };

export interface CapacityRow {
  bucket: string;
  idle: number;
  busy: number;
  reserved: number;
  available: number;
  summed_capacity?: number;
}

export interface CapacityQuery {
  matcher?: TagMatcher;
  group_by: GroupBy;
  max_rtt_ms?: number;
  sum_axis_key?: string;
  limit: number;
}

export class CapabilityFoldHandle {
  aggregate(matcher: TagMatcher | undefined, groupBy: GroupBy, agg: Aggregation):
    Array<[string, number]>;
  capacityRanking(
    query: CapacityQuery,
    rttLookup: (nodeId: bigint) => number | undefined,
  ): CapacityRow[];
}
```

**Tests (`sdk-ts/test/fold-capabilities.test.ts`):**
- vitest pinning each `TagMatcher` / `GroupBy` variant's JSON shape.
- End-to-end test: populate a fold via the publisher path, run
  `capacityRanking` with a JS closure that returns canned RTTs,
  assert the row shape + ordering.

### Python bindings (`bindings/python` + `sdk-py`)

**Wire shape:**
- PyO3 wraps `TagMatcher` / `GroupBy` / `Aggregation` / `CapacityQuery` /
  `CapacityRow` as `#[pyclass]` types. Each enum variant becomes a
  classmethod constructor (e.g. `TagMatcher.prefix("hardware.gpu")`).
- `rtt_lookup` is a Python callable. PyO3's `PyAny` lets the Rust
  closure call back into Python; the GIL is held for the duration
  of the lookup. Sync, like Node — the aggregation is fast enough
  that holding the GIL across the rtt loop is acceptable.
- Results return as `list[CapacityRow]` (a `pyclass` with
  `idle: int`, `busy: int`, etc.).

**Python surface (`sdk-py/src/net_sdk/capabilities/aggregation.py`):**

```python
from net_sdk import TagMatcher, GroupBy, CapacityQuery, CapacityRow

# Top 5 GPU types within 50ms latency
rows = fold.capacity_ranking(
    CapacityQuery(
        matcher=TagMatcher.prefix("hardware.gpu"),
        group_by=GroupBy.tag_stem("hardware.gpu"),
        max_rtt_ms=50,
        sum_axis_key="hardware.gpu.count",
        limit=5,
    ),
    rtt_lookup=lambda node_id: proximity.rtt_ms_to(node_id),
)
for row in rows:
    print(f"{row.bucket:>20} {row.idle} idle / {row.available} total")
```

**Tests:**
- pytest assertions on every variant's classmethod.
- End-to-end with a Python rtt_lookup callable.

### Go bindings (`bindings/go/compute-ffi` or new `bindings/go/fold-ffi`)

**ABI choice:** Go's CGo callback story is workable but slow (each
callback crosses the Go-C boundary). For the aggregation path, two
approaches:

1. **Materialized RTT map** (recommended for Go + C): operator
   builds a Go `map[uint64]uint32` of RTTs ahead of time and passes
   it as a serialized (postcard or protobuf) blob through FFI. Rust
   uses it as a HashMap lookup. Trade-off: caller pays for every
   node's RTT once even if only a few survive the matcher filter;
   in practice operators have the RTT map cached anyway from the
   proximity graph.
2. **CGo callback**: register a Go function as a C function
   pointer, marshal node_id through the ABI. Works but adds ~100ns
   per call vs ~1ns for the materialized map.

Recommend approach 1.

**Go surface (`go/capability/aggregation.go`):**

```go
package capability

type TagMatcher struct { /* tagged union as a struct with one field set */ }
type GroupBy struct { /* same shape */ }
type CapacityQuery struct {
    Matcher        *TagMatcher
    GroupBy        GroupBy
    MaxRTTMillis   *uint32
    SumAxisKey     *string
    Limit          int
}
type CapacityRow struct {
    Bucket           string
    Idle, Busy       uint64
    Reserved         uint64
    Available        uint64
    SummedCapacity   *uint64
}

func (f *Fold) CapacityRanking(
    query CapacityQuery,
    rttLookup map[uint64]uint32,
) ([]CapacityRow, error)
```

**Tests:** `go test ./capability/...` with table-driven cases for
each matcher / group_by variant.

### C SDK (`net.h` headers + `crates/net/cli`-adjacent C ABI)

**ABI shape:** flat C structs, opaque pointers, materialized RTT map
(per the Go rationale).

**Header sketch (`net/include/net_fold.h`):**

```c
typedef enum {
    NET_TAG_MATCH_EXACT,
    NET_TAG_MATCH_PREFIX,
    NET_TAG_MATCH_AXIS,
    NET_TAG_MATCH_AXIS_KEY,
    NET_TAG_MATCH_REGEX,         // Phase 6c-C
    NET_TAG_MATCH_VERSION_RANGE, // Phase 6c-C
} net_tag_matcher_kind_t;

typedef struct {
    net_tag_matcher_kind_t kind;
    const char* value;      // for Exact / Prefix / Regex
    uint8_t axis;           // for Axis / AxisKey (TaxonomyAxis as u8)
    const char* key;        // for AxisKey
    const char* min;        // for VersionRange
    const char* max;        // for VersionRange
} net_tag_matcher_t;

// ... GroupBy, CapacityQuery, CapacityRow analogously ...

typedef struct {
    uint64_t node_id;
    uint32_t rtt_ms;
} net_rtt_entry_t;

// Returns 0 on success, fills `out_rows` (caller-owned buffer).
// `rtt_entries == NULL` disables the RTT filter.
int net_fold_capacity_ranking(
    net_fold_handle_t* fold,
    const net_capacity_query_t* query,
    const net_rtt_entry_t* rtt_entries,
    size_t rtt_entries_len,
    net_capacity_row_t* out_rows,
    size_t* out_rows_len,
    char* err_buf,
    size_t err_buf_len
);
```

**Tests:** add to the existing C ABI smoke test (if any) or a new
small C harness under `bindings/c/test/`.

---

## Cross-binding parity table

| Surface | Rust | TS | Python | Go | C |
|---|---|---|---|---|---|
| `TagMatcher` variants A-D (6c-A) | ✓ | ✓ | ✓ | ✓ | ✓ |
| `TagMatcher::Regex/VersionRange` (6c-C) | ✓ | ✓ | ✓ | ✓ | ✓ |
| `GroupBy` variants | ✓ | ✓ | ✓ | ✓ | ✓ |
| `Aggregation::{Count, Distinct*}` | ✓ | ✓ | ✓ | ✓ | ✓ |
| `Aggregation::SumNumericTag` | ✓ | ✓ | ✓ | ✓ | ✓ |
| `aggregate(...)` | ✓ | ✓ | ✓ | ✓ | ✓ |
| `capacity_ranking(...)` with rtt closure | ✓ | ✓ (TSF) | ✓ (callable) | – | – |
| `capacity_ranking(...)` with rtt map | – | – | – | ✓ | ✓ |

Closure-based RTT works in Rust/TS/Python where the binding can call
back into the host language cheaply. Go/C ship the materialized-map
variant for ABI simplicity. The Rust core ships both adapters:
`capacity_ranking_with_lookup` (closure) and
`capacity_ranking_with_map` (HashMap) so each binding picks the
shape it can serve.

---

## Tests strategy

### Framework (Rust core, 6c-A/B/C)

Per-variant unit tests in `behavior/fold/capability.rs` covering:

- Each `TagMatcher` variant produces the right entry-set filter.
- Each `GroupBy` variant buckets correctly + handles the "(none)"
  case for missing region.
- Each `Aggregation` variant computes the right per-bucket value.
- `aggregate` with no matcher returns every entry's bucket.
- `aggregate` with a matcher that excludes everything returns
  empty.
- `capacity_ranking` excludes Faulty entries from `available`.
- `capacity_ranking` honors `max_rtt_ms` (entries with
  `rtt_lookup(id) > max` are dropped; entries with
  `rtt_lookup(id) == None` are dropped).
- `capacity_ranking` returns rows sorted by `available` descending
  + truncated to `limit`.
- `sum_axis_key` parses numeric tag values and sums per bucket;
  unparseable values are skipped (logged at trace, not surfaced as
  errors).

Target: ~20 framework tests across 6c-A through 6c-C.

### Bindings

Each binding ships its own test suite mirroring the framework's
assertions. The shared property: every binding's
`capacity_ranking(query, rtt_lookup)` returns the same logical
result as the Rust core for the same (query, rtt_lookup) input.

---

## Risks and mitigations

**Risk: regex / semver deps inflate the binary.** `regex` is ~1MB
static; `semver` is small. **Mitigation:** gate Phase 6c-C behind a
Cargo feature (`fold-advanced-matchers`) defaulted to on. Operators
on bandwidth-constrained binaries can build without.

**Risk: `rtt_lookup` closure called once per candidate → O(N)
callbacks across FFI for TS/Python.** **Mitigation:** the closure
fires only AFTER the matcher filter narrows entries, so N is
typically small (10s-100s of candidates after a tag-prefix filter).
Document the expectation; if profile shows it's a hotspot in some
binding, switch that binding to the materialized-map variant.

**Risk: `sum_axis_key` over-allocates if tag values are huge.**
**Mitigation:** parsing failures are skipped silently; numeric
overflow saturates rather than panicking.

**Risk: `TagStem` group-by produces a long-tail of singleton
buckets** (e.g. `model.*` with 200 publishers each on a different
fine-tuned variant). **Mitigation:** the `limit` parameter caps
output; operators tune it. Could add a `min_count_threshold` field
in a follow-up if real workloads need it.

**Risk: cross-binding behavior drift.** The five bindings produce
the same logical result for the same input by construction (all
delegate to the Rust core). **Mitigation:** the shared
property test above. If a binding's wrapper introduces a bug
(e.g. wrong JSON encoding), the test catches it; the Rust core is
untouched.

---

## Sign-off checklist

- [ ] 6c-A: framework `aggregate` + 4 matcher variants + 6 group_by
      variants + 3 aggregation variants + 7+ unit tests pass.
- [ ] 6c-B: `capacity_ranking` + `sum_axis_key` + `SumNumericTag`
      + 8+ unit tests pass; benchmark on a 1000-entry fold under 1ms p99.
- [ ] 6c-C: `Regex` + `VersionRange` + `Min/MaxNumericTag` + 5+
      unit tests; `regex` and `semver` deps added behind feature gate.
- [ ] 6c-D: Rust SDK re-exports + 3 doctests pass.
- [ ] 6c-E: Node bindings + sdk-ts + vitest passes; pinned shape
      against framework reference.
- [ ] 6c-F: Python bindings + sdk-py + pytest passes; pinned shape.
- [ ] 6c-G: Go bindings + `go test` passes; materialized-map RTT
      variant tested.
- [ ] 6c-H: C ABI + smoke test passes; materialized-map variant.

All sub-steps independent post-6c-C: bindings ship as their own
PRs against a stable Rust core.
