# Capability System Migration Guide

How to update application code for the typed-tag capability surface
(Phase A.5.N of `CAPABILITY_SYSTEM_PLAN.md` + the enhancement track in
`CAPABILITY_ENHANCEMENTS_PLAN.md`).

This guide covers the breaking changes from the legacy
`CapabilitySet { hardware: HardwareCapabilities, software: ..., ... }`
shape to the canonical `CapabilitySet { tags: HashSet<Tag>, metadata:
BTreeMap<String, String> }` wire format, plus the new
caller-local APIs that replace direct field access.

## TL;DR

| Old (pre-Phase A.5.N)              | New                                  |
| ---------------------------------- | ------------------------------------ |
| `caps.hardware.gpu`                | `caps.views().hardware().gpu`        |
| `caps.software.os`                 | `caps.views().software().os`         |
| `caps.models[0]`                   | `caps.views().models()[0]`           |
| `caps.tools[0]`                    | `caps.views().tools()[0]`            |
| `caps.limits`                      | `caps.views().resource_limits()`     |
| `caps.tags` (`Vec<String>`)        | `caps.tags` (`HashSet<Tag>`)         |
| (no metadata)                      | `caps.metadata` (`BTreeMap<String, String>`) |

The wire format is now `{ tags, metadata }` only. Callers that need
typed projections (hardware shape, software shape, etc.) reach for
`caps.views()`, which decodes the tag set lazily and caches per-axis
projections via `OnceCell` (Rust) / `sync.Once` (Go) / lazy property
(TS / Python). Decoding cost is paid once per axis per `views()`
handle; subsequent reads are < 50 ns.

> **No backward-compat shim.** The substrate broke wire format
> intentionally. Peers must upgrade together; mixed-version meshes
> drop unrecognized fields silently and produce wrong matches.
> See `CAPABILITY_SYSTEM_PLAN.md` Locked decision §
> "No backward-compat shim" for the rationale.

## Migration patterns

### 1. Building a capability set

**Old (typed-struct constructors)**

```rust
let caps = CapabilitySet::new()
    .with_hardware(HardwareCapabilities::new()
        .with_cpu(16, 32)
        .with_memory(64)
        .with_gpu(GpuInfo::new(GpuVendor::Nvidia, "h100", 80)))
    .with_software(SoftwareCapabilities::new()
        .with_os("linux", "6.6")
        .add_runtime("python", "3.11"));
```

**New (chain helpers + tag-set wire format)**

```rust
use net_sdk::capabilities::{CapabilitySet, HardwareCapabilities, SoftwareCapabilities, GpuInfo, GpuVendor};

// The typed-struct constructors still exist on the substrate's
// `HardwareCapabilities` / `SoftwareCapabilities` types — they're now
// internally tag-encoded via the tag_codec helpers.
let caps = CapabilitySet::new()
    .with_hardware(HardwareCapabilities::new()
        .with_cpu(16, 32)
        .with_memory(64)
        .with_gpu(GpuInfo::new(GpuVendor::Nvidia, "h100", 80)))
    .with_software(SoftwareCapabilities::new()
        .with_os("linux", "6.6")
        .add_runtime("python", "3.11"));

// Or use the wire-shape builders directly via `add_tag` (which
// parses through `Tag::parse_user`, gating reserved prefixes) and
// the metadata setter:
let caps = CapabilitySet::new()
    .add_tag("hardware.gpu=h100")
    .add_tag("hardware.memory_gb=64")
    .with_metadata("intent", "ml-training");
```

In TS:

```ts
import {
  emptyCapabilities, requireTag, requireAxisValue, withMetadata,
} from '@net-mesh/sdk';

const caps = withMetadata(
  requireAxisValue(
    requireTag(emptyCapabilities(), 'hardware', 'gpu'),
    'software', 'os', 'linux',
  ),
  'intent', 'ml-training',
);
// caps = { tags: ['hardware.gpu', 'software.os=linux'], metadata: { intent: 'ml-training' } }
```

In Python:

```python
from net_sdk import (
    empty_capabilities, require_tag, require_axis_value, with_metadata,
)

caps = with_metadata(
    require_axis_value(
        require_tag(empty_capabilities(), "hardware", "gpu"),
        "software", "os", "linux",
    ),
    "intent", "ml-training",
)
```

In Go:

```go
caps := EmptyCapabilities()
caps, _ = RequireTag(caps, AxisHardware, "gpu")
caps, _ = RequireAxisValue(caps, AxisSoftware, "os", "linux", SepEq)
caps, _ = WithMetadata(caps, "intent", "ml-training")
```

### 2. Reading typed fields → `views()`

**Old (direct field access)**

```rust
if caps.hardware.gpu.is_some() {
    let vram = caps.hardware.gpu.as_ref().unwrap().vram_gb;
    // ...
}
let os = &caps.software.os;
let n_models = caps.models.len();
```

**New (`views()` with cached lazy projections)**

```rust
let v = caps.views();
if v.hardware().gpu.is_some() {
    let vram = v.hardware().gpu.as_ref().unwrap().vram_gb;
}
let os = &v.software().os;
let n_models = v.models().len();
```

Key points:

- `views()` is **cheap** — it returns a borrow handle with empty
  `OnceCell`s. The actual decode (axis-specific tag-codec walk) only
  happens on first access of each projection.
- The handle borrows from `caps`. Don't mutate `caps` while `views()`
  is alive.
- Calling `v.hardware()` does NOT force the `software` / `models` /
  `tools` / `resource_limits` decoders. Each axis is independent.
- For one-shot reads, `let hw = HardwareCapabilities::from(&caps);`
  still works (allocates a fresh decode); use `views()` when you
  need multiple projections from the same set.

In TS / Python / Go, the views projections are not yet shipped (Phase
3 follow-up). Application code still constructs `caps` via the
typed-struct constructors and reads typed fields back through them:

```ts
// TS — typed fields still live on the POJO shape; the SDK's
// announceCapabilities / findNodes accept them and round-trip
// through the substrate.
await node.announceCapabilities({
  hardware: { cpuCores: 16, memoryGb: 64, gpu: { vendor: 'nvidia', model: 'h100', vramGb: 80 } },
  tags: ['inference'],
});
```

When `views()` lands in the host bindings (Phase 3 follow-up), the
migration path will be:

```ts
// future
const v = caps.views();
console.log(v.hardware.gpu?.vramGb);
```

### 3. Tag round-tripping

**Old (string tags)**

```rust
caps.tags.contains(&"gpu".to_string())
```

**New (typed `Tag` set, Display matches the wire string)**

```rust
caps.tags.iter().any(|t| t.to_string() == "hardware.gpu")
// or use the predicate evaluator (preferred):
let pred = pred!(exists "hardware.gpu");
// Materialize the tag slice into a local first — `EvalContext::new`
// borrows it, and a temporary `.collect::<Vec<_>>()` chained inline
// would be dropped before `evaluate_unplanned` runs.
let tag_vec: Vec<Tag> = caps.tags.iter().cloned().collect();
let ctx = EvalContext::new(&tag_vec, &caps.metadata);
pred.evaluate_unplanned(&ctx)
```

The wire form of a `Tag` is the `Display` string:

| Tag variant         | Wire form                       |
| ------------------- | ------------------------------- |
| `AxisPresent`       | `<axis>.<key>` (e.g. `hardware.gpu`) |
| `AxisValue`         | `<axis>.<key>=<value>` or `<axis>.<key>:<value>` |
| `Reserved`          | `<prefix><body>` (e.g. `scope:tenant:foo`) |
| `Legacy`            | arbitrary string (forward-compat) |

User code that produces tags should go through `Tag::parse_user(s)`
(rejects reserved prefixes) or the binding's equivalent
(`tagFromUserString` in TS, `tag_from_user_string` in Python,
`TagFromUserString` in Go). Substrate-privileged paths use
`Tag::parse(s)`.

### 4. New surface: `caps.diff(prev)`

Detect what changed between two capability snapshots — useful for
placement re-evaluation when a daemon's `CapabilitySet` updates.

```rust
let diff = curr.diff(&prev);
// diff.added_tags   : Vec<&Tag>     — tags in curr but not prev
// diff.removed_tags : Vec<&Tag>     — tags in prev but not curr
// diff.changed_metadata : Vec<MetadataChange>
//   { Added { key, value } | Removed { key, prev_value }
//     | Updated { key, prev_value, new_value } }
```

A metadata key rename is reported as `Removed` + `Added`, NOT
`Updated` — key identity changes are semantically distinct from value
changes. Pinned by the cross-binding `capability_set_diff.json`
fixture.

Equivalent across bindings: `diffCapabilities(prev, curr)` (TS),
`diff_capabilities(prev, curr)` (Python),
`DiffCapabilities(prev, curr)` (Go).

### 5. New surface: predicate language

The substrate's `Predicate` AST replaces ad-hoc `CapabilityFilter`
matchers for non-trivial queries:

```rust
use net_sdk::capabilities::pred;

let p = pred!(and [
    pred!(exists "hardware.gpu"),
    pred!(num_at_least "hardware.memory_gb", 64.0),
    pred!(metadata_equals "intent", "ml-training"),
]);

// Local evaluation (no index lookup):
let ctx = EvalContext::new(&tags, &metadata);
let matched = p.evaluate(&ctx);

// Or push the predicate over nRPC. `predicate_to_rpc_header`
// returns a `Result<(name, value), _>` pair — destructure both
// halves; the name is the canonical `RPC_WHERE_HEADER` constant.
use net_sdk::capabilities::predicate::predicate_to_rpc_header;
let (name, value) = predicate_to_rpc_header(&p)?;
// ...attach `(name, value)` as a request header.
```

In TS:

```ts
import { p, predicateToRpcHeader, RPC_WHERE_HEADER, evaluatePredicate } from '@net-mesh/sdk';

const pred = p.and(
  p.exists({ axis: 'hardware', key: 'gpu' }),
  p.numericAtLeast({ axis: 'hardware', key: 'memory_gb' }, 64),
  p.metadataEquals('intent', 'ml-training'),
);

const matched = evaluatePredicate(pred, tags, metadata);
const headerValue = predicateToRpcHeader(pred);
```

The wire format is **byte-identical** across all four bindings —
pinned by `predicate_nrpc_envelope.json`. A predicate authored in TS
and passed to a Go service via nRPC headers decodes losslessly on the
other end.

#### Predicate-pushdown via `mesh.call`

The Rust SDK ships an end-to-end predicate-pushdown path on top of
nRPC (Phase 9b):

```rust
use net_sdk::mesh_rpc::{CallOptions, CallOptionsExt, RpcContext, RpcContextExt};

// Caller side: attach the predicate as a `net-where:` request
// header. `with_where` returns `Result` because the predicate's
// JSON encoding is bounded by `MAX_PREDICATE_RPC_HEADER_VALUE_LEN`.
let opts = CallOptions::default()
    .with_where(&pred)
    .expect("predicate fits");

// Server side: read the predicate from the request context.
async fn handle(ctx: RpcContext) -> Result<RpcResponsePayload, RpcHandlerError> {
    let pred = ctx.where_predicate()
        .map(|r| r.expect("decode")) // None when absent; Err(_) on malformed wire
        .unwrap_or_else(|| /* default: no filter */ Predicate::and(vec![]));
    // ...filter result set against `pred`...
}
```

The substrate's `CallOptions::request_headers` field carries
arbitrary `(name, value)` pairs alongside the standard nRPC envelope;
`with_where` is sugar over destructuring the encoder's tuple and
forwarding to `with_request_header`:

```rust
let (name, value) = predicate_to_rpc_header(&pred)?;
options.with_request_header(name, value)
```

Per-binding wrappers in TS / Python / Go expose the same
`net-where:` header convention.

### 6. New surface: validation

`validate_capabilities(caps)` returns a `ValidationReport` flagging
schema violations + forward-compat warnings:

```rust
use net_sdk::capabilities::schema::validate_capabilities;

let report = validate_capabilities(&caps);
if !report.is_valid() {
    for err in &report.errors {
        eprintln!("schema error: {err:?}");
    }
}
for warn in &report.warnings {
    eprintln!("warning: {warn:?}");
}
```

`SchemaError` variants: `UnknownAxis`, `TypeMismatch`,
`IndexMalformed`. `ValidationWarning` variants: `UnknownKey`,
`MetadataOversize`, `LegacyTag`. Each binding ships an equivalent
`validateCapabilities` / `validate_capabilities` /
`ValidateCapabilities` returning the same wire shape (pinned by
`capability_validation.json`).

### 7. New surface: predicate trace + debug reports

Diagnose why a predicate did / didn't match — per-clause hit / miss
stats across a corpus:

```rust
let report = PredicateDebugReport::from_evaluations(&pred, contexts);
println!("{}", report.render());
```

In TS:

```ts
import { predicateDebugReport, redactMetadataKeys, renderDebugReport } from '@net-mesh/sdk';

const report = predicateDebugReport(pred, contexts);

// Optional: scrub sensitive metadata values before persisting.
const safe = redactMetadataKeys(report, ['intent', 'owner']);
console.log(renderDebugReport(safe));
```

`render()` produces a per-clause breakdown:

```
Predicate evaluation report
─────────────────────────────────────────
Total candidates: 1042
Matched:          12 (1.2%)

Per-clause stats (alphabetical):
  Exists(hardware.gpu)                                         evaluated  1042, matched   712 ( 68.3%)
  MetadataEquals(intent=ml-training)                           evaluated  1042, matched   312 ( 29.9%)
  ...
```

Operators read these to spot mismatches between their mental model of
the data and the actual data. Round-trip via JSON: `report.toWire()`
+ `predicateDebugReportFromWire(JSON.parse(text))`.

## What did NOT change

These surfaces stayed stable:

- `Mesh::announce_capabilities(caps)` — still takes a `CapabilitySet` and returns `Result<()>` (async).
- `Mesh::find_nodes(filter)` — still takes a `CapabilityFilter` (the
  old query API). For richer queries, use the new predicate AST via
  `find_nodes_matching` (substrate Phase 4 of enhancements).
- `Mesh::find_nodes_scoped(filter, scope)` — unchanged.
- The substrate's `Identity`, `Subnet`, `Channel`, `Token` surfaces.
- nRPC core (`MeshRpc::call` / `MeshRpc::serve` / `TypedMeshRpc`).

## Compatibility shims

There are none. `Cargo.toml` callers updating to ≥ 0.12.0 must update
their type imports:

| Old import path                    | New                                  |
| ---------------------------------- | ------------------------------------ |
| `net::capabilities::CapabilitySet` | `net_sdk::capabilities::CapabilitySet` |
| `net::capabilities::Tag`           | `net_sdk::capabilities::Tag` |
| `net::capabilities::CapabilityFilter` | `net_sdk::capabilities::CapabilityFilter` |
| (new)                              | `net_sdk::capabilities::predicate::Predicate` |
| (new)                              | `net_sdk::capabilities::schema::validate_capabilities` |

The substrate's internal modules (`net::adapter::net::behavior::*`)
are NOT a stable surface — go through `net_sdk::capabilities::*`.

## Cross-binding fixtures

The shape contracts you can rely on:

| Fixture (`tests/cross_lang_capability/`) | What it pins                       |
| ---------------------------------------- | ---------------------------------- |
| `predicate_nrpc_envelope.json`           | `Predicate` ↔ `net-where` JSON header |
| `capability_set_diff.json`               | `CapabilitySet::diff(prev)` output  |
| `predicate_eval.json`                    | `Predicate::evaluate_unplanned(ctx)` boolean |
| `predicate_trace.json`                   | `Predicate::evaluate_with_trace(ctx)` tree |
| `predicate_debug_report.json`            | `PredicateDebugReport::from_evaluations` |
| `predicate_debug_report_redacted.json`   | `redactMetadataKeys(report, keys)` output |
| `capability_validation.json`             | `validate_capabilities(caps)` output |

A binding that decodes any of these fixtures and re-encodes via its
host-language types must produce byte-identical output. Drift in any
fixture is a P0: a Node client and a Go service stop interoperating.

## See also

- `CAPABILITY_SYSTEM_PLAN.md` — substrate-side migration plan.
- `CAPABILITY_ENHANCEMENTS_PLAN.md` — caller-local enhancement track
  (lazy projections, predicate AST, validation, diff, debug session).
- `CAPABILITY_SYSTEM_SDK_PLAN.md` — per-binding SDK rollout plan.
- `CAPABILITIES_SCHEMA.md` — canonical schema doc; binding generators
  read this and produce equivalent host-language schemas.
- `CAPABILITY_ENHANCEMENTS_USAGE.md` — worked-examples guide for the
  enhancement APIs.
