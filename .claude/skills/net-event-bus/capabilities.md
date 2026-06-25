# Capabilities — Capability-Based Routing

Read this when the user wants to **place compute on the right node**, not push events through a topic. The pattern: each node declares what it can do (GPU model, loaded LLMs, callable tools, free-form tags), other nodes query the mesh by filter, and you get back a list of node ids you can talk to directly. Killed by the routed-handshake path so it works across NAT.

This is the primitive Kafka / NATS / Redis Streams / Pulsar do not have. There is no broker subject like `gpu-work` you subscribe to and hope something with a GPU is listening; you ask the mesh "who has an NVIDIA GPU with ≥ 16 GB VRAM and `llama-3.1-70b` already loaded?" and unicast to the winner.

If the user wants topic-style fan-out, use `apis.md` (named channels) or `patterns.md` § "I want a consumer that subscribes to a topic". This file is for placement.

`find_nodes` is **advisory** — it answers "who *can* do X", with no exclusivity; two callers get the same list. If the user needs to **atomically claim a contended exclusive resource** (N jobs want the same GPU island / slot / seat; exactly one must win, no double-booking), that's `scheduler.md` — capability discovery here, then the contended CAS there.

---

## The capability model

Each node builds a `CapabilitySet` and announces it. Storage shape:

```rust
pub struct CapabilitySet {
    pub tags: HashSet<Tag>,
    pub metadata: BTreeMap<String, String>,
}
```

`Tag` is a four-axis typed enum:

- `Tag::AxisPresent { axis, key }` — boolean axis tag (`hardware.gpu`).
- `Tag::AxisValue { axis, key, value, separator }` — keyed axis tag (`hardware.gpu.vram_gb=24`, `software.os:linux`). The substrate accepts `=` or `:` as separator and stores it on the wire; semantic equality ignores the separator (`software.os=linux` and `software.os:linux` match).
- `Tag::Reserved { prefix, body }` — reserved cross-axis tag (`scope:tenant:foo`, `causal:<hex>`, `fork-of:<hex>`, `heat:warm`). Only substrate code emits these; `Tag::parse_user` rejects reserved prefixes from application input.
- `Tag::Legacy(String)` — untyped pre-Phase-A free-form (`nat:full-cone`, `nrpc:my-service`).

Hardware / software / model / tool / resource-limit views are *projections* of the tag set, lazily decoded via `caps.views()`:

- `HardwareCapabilities`: `cpu_cores`, `cpu_threads`, `memory_gb`, `gpu: Option<GpuInfo>`, `additional_gpus`, `storage_gb`, `network_gbps`, `accelerators` — encoded as `hardware.cpu_cores=N` / `hardware.gpu` / `hardware.gpu.vram_gb=N` / etc.
- `GpuInfo`: `vendor: GpuVendor`, `model`, `vram_gb`, `compute_units`, `tensor_cores`, `fp16_tflops_x10`.
- `ModelCapability`: `model_id`, `family`, `parameters_b_x10`, `context_length`, `quantization`, `modalities`, `tokens_per_sec`, `loaded` — indexed-encoded as `software.model.0.id=...` / `software.model.0.family=...` / etc.
- `ToolCapability`: `tool_id`, `name`, `version`, `input_schema`, `output_schema`, `requires`, `estimated_time_ms`, `stateless`. Schemas live in `metadata` under `tool::<id>::input_schema` / `tool::<id>::output_schema` (the JSON-Schema strings can't safely round-trip through the tag wire format).
- `ResourceLimits`: `max_concurrent_requests`, `max_tokens_per_request`, `rate_limit_rpm`, `max_batch_size`, `max_input_bytes`, `max_output_bytes` — encoded under `hardware.limits.*`.

`GpuVendor` is `Nvidia | Amd | Intel | Apple | Qualcomm | Unknown`. `Modality` is `Text | Image | Audio | Video | Code | Embedding | ToolUse`. `parameters_b_x10` and `fp16_tflops_x10` are integer-encoded (× 10) to dodge float precision loss on the wire. Free-form strings the user wants to ride alongside (`prod`, `customer:acme`, `gpu-pool-a`) parse as `Tag::Legacy` via `add_tag` / `tag_from_user_string`.

The builders haven't changed — `CapabilitySet::new().with_hardware(...).add_model(...).add_tag("prod")` writes through to the canonical tag/metadata shape under the hood. Reads via `caps.views().hardware()` decode on first call and cache.

---

## Announcing

`announce_capabilities(caps)` pushes the set to every directly-connected peer over subprotocol `0x0C00` (`net/crates/net/src/adapter/net/behavior/broadcast.rs:12`). The announcer self-indexes too, so single-node tests round-trip. Multi-hop propagation is deferred — peers more than one hop away will not see the announcement today.

Re-announce to update. Subsequent calls go through the `CapabilityDiff` machinery in `net/crates/net/src/adapter/net/behavior/diff.rs` — incremental, signed — so steady-state changes (a model gets loaded, a tag toggles) cost ~50 bytes on the wire instead of a full re-broadcast. You don't drive this directly; the mesh figures it out from your last announced set.

Default TTL is 5 minutes. Override with `announce_capabilities_with(caps, ttl, sign)` (Rust). Re-announce before TTL elapses or peers will GC the entry.

**Announcements are bound to the signer.** The capability fold binds an announcement's wire `node_id` to the verified signing key, so a peer cannot announce capabilities *as* another node. A forged `node_id` (one that doesn't match the signer) is rejected with `WireError::NodeIdMismatch` rather than silently folded in.

---

## Querying — three shapes

| Shape | Returns | When to use |
|---|---|---|
| `find_nodes(filter)` | every node id whose latest announcement matches | broadcast a request to all candidates, or hand-pick |
| `find_best_node(req)` | a single node id, the highest-scoring match | "give me one node, optimize for these weights" — the workload-scheduler pattern |
| `find_nodes_scoped(filter, scope)` | matches narrowed to a tenant / region / subnet / global pool | multi-tenant or multi-region deployments |

`find_best_node` ranks via `CapabilityRequirement` (`capability.rs:1367`):

```rust
pub struct CapabilityRequirement {
    pub filter: CapabilityFilter,
    pub prefer_more_memory: f32,
    pub prefer_more_vram: f32,
    pub prefer_faster_inference: f32,
    pub prefer_loaded_models: f32,
}
```

Weights are clamped to `[0.0, 1.0]`. `prefer_loaded_models = 1.0` is the right setting for "don't pay cold-start latency" — a node that already has the model loaded wins over an idle GPU with more VRAM.

`CapabilityFilter` (`capability.rs:1207`) — what every shape filters by:

```rust
pub struct CapabilityFilter {
    pub require_tags: Vec<String>,
    pub require_models: Vec<String>,
    pub require_tools: Vec<String>,
    pub min_memory_gb: Option<u32>,
    pub require_gpu: bool,
    pub gpu_vendor: Option<GpuVendor>,
    pub min_vram_gb: Option<u32>,
    pub min_context_length: Option<u32>,
    pub require_modalities: Vec<Modality>,
}
```

`require_tags` is AND (every tag must be present). `require_models` and `require_tools` are OR (any one match satisfies).

**Queries are index-driven and cheap.** The bulk-query path returns `NodeId`s directly rather than cloning each candidate's `CapabilityMembership` payload, and complex `query_model` / `query_tool` filters are seeded from the tag index instead of full-scanning. On a 10k-node fold (M1 Max) single-tag lookups run in ~184 µs and `query_model` in ~88 µs; concurrent queries parallelize through the dual-`RwLock`-read structure. You still shouldn't rebuild a topic system on `find_nodes` (latest-wins, no ordering — see *What this is NOT*), but per-query cost is no longer the reason not to.

---

## Predicates — the richer query surface

`CapabilityFilter` is the field-comparison surface; it's deliberately narrow. For arbitrary boolean queries (semver compatibility, regex on values, metadata thresholds, AND/OR/NOT compositions) every binding ships a typed `Predicate` AST. Same wire format across Rust / TS / Python / Go / C — pinned by the JSON fixtures under `tests/cross_lang_capability/`.

```rust
use net_sdk::capabilities::{
    p, evaluate_predicate, predicate_to_rpc_header,
    validate_capabilities, tag_key,
};

let pred = p.and(&[
    p.exists(&tag_key("hardware", "gpu")),
    p.numeric_at_least(&tag_key("hardware", "memory_gb"), 64.0),
    p.semver_compatible(&tag_key("software", "runtime.python"), "3.11.0"),
    p.metadata_equals("intent", "ml-training"),
]);

// Local evaluation against any (tags, metadata).
let matched = evaluate_predicate(&pred, &caps.tags, &caps.metadata);

// Wire form for nRPC `net-where:` headers — pair with the
// header-bearing call variants so the server short-circuits
// candidates without re-running the predicate per hop.
let header_value = predicate_to_rpc_header(&pred);

// Validate a CapabilitySet against the canonical schema before
// announcing — catches typos, type mismatches, oversize metadata,
// legacy-tag warnings.
let report = validate_capabilities(&caps);
if !report.is_valid() { /* report.errors */ }

// Detect what changed between two snapshots.
let delta = caps.diff(&prev);

// Single-evaluation trace + corpus aggregation with optional
// metadata-key redaction before persistence.
let (result, trace) = evaluate_predicate_with_trace(&pred, &tags, &metadata);
let report = predicate_debug_report(&pred, &corpus);
let safe = redact_metadata_keys(&report, &["intent"]);
```

Identical surface in TS (`p.and`, `p.exists`, `evaluatePredicate`, `predicateToRpcHeader` / `predicateFromRpcHeader`, `validateCapabilities`, `diffCapabilities`, `evaluatePredicateWithTrace`, `predicateDebugReport`, `redactMetadataKeys`), Python (`p.and_`, `p.exists`, `evaluate_predicate`, …), Go (`Predicate{}`, `EvaluatePredicate`, `PredicateToWhereHeader`), and C (`net_predicate_evaluate`, `net_validate_capabilities`, `net_predicate_to_where_header`, `net_predicate_evaluate_with_trace`, `net_predicate_aggregate_debug_report`, `net_predicate_redact_metadata_keys`). A predicate authored in TS and shipped to a Go server via the header decodes losslessly.

**Placement-filter callbacks (Phase 7).** When the substrate's built-in scoring axes don't fit your placement rule, plug a host-language predicate in via `placement_filter_from_fn(...)` (Rust SDK / TS / Python / Go) — the substrate calls back per candidate. Pair with `standard_placement(custom_filter_id=...)` so the daemon-placement scheduler weights your callback alongside its native axes. C consumers reach the same dispatcher via `net_compute_set_placement_filter_dispatcher` + `net_compute_register_placement_filter` (`include/net.go.h`).

---

## Per-SDK examples

### Rust

```rust
use net_sdk::capabilities::{
    CapabilityFilter, CapabilityRequirement, CapabilitySet, GpuInfo, GpuVendor,
    HardwareCapabilities,
};
use net_sdk::mesh::MeshBuilder;

let node = MeshBuilder::new("0.0.0.0:0", &psk).build().await?;
let hw = HardwareCapabilities::new()
    .with_cpu(16, 32)
    .with_memory(64)
    .with_gpu(GpuInfo::new(GpuVendor::Nvidia, "RTX 4090", 24));
node.announce_capabilities(CapabilitySet::new().with_hardware(hw).add_tag("prod")).await?;

let req = CapabilityRequirement::from_filter(
    CapabilityFilter::new().with_gpu_vendor(GpuVendor::Nvidia).with_min_vram(16),
);
if let Some(node_id) = node.find_best_node(&req) { /* unicast to node_id */ }
```

**Key facts:**
- Public types live under `net_sdk::capabilities::*` (re-exports in `net/crates/net/sdk/src/capabilities.rs:62-67`).
- Mesh wrapper is `net_sdk::mesh::Mesh`, built via `MeshBuilder`. All five methods (`announce_capabilities`, `announce_capabilities_with`, `find_nodes`, `find_nodes_scoped`, `find_best_node`, `find_best_node_scoped`) live on it (`net/crates/net/sdk/src/mesh.rs:714-775`).
- `ScopeFilter<'a>` borrows its tenant / region strings — keep them alive across the call.

### TypeScript

```typescript
import { MeshNode, normalizeGpuVendor } from '@net-mesh/sdk';

const node = await MeshNode.create({ bindAddr: '0.0.0.0:0', psk });
await node.announceCapabilities({
  hardware: {
    cpuCores: 16, memoryGb: 64,
    gpu: { vendor: 'nvidia', model: 'RTX 4090', vramGb: 24 },
  },
  tags: ['prod'],
  models: [{ modelId: 'llama-3.1-70b', family: 'llama', loaded: true }],
});
const peers: bigint[] = node.findNodes({
  requireGpu: true, gpuVendor: normalizeGpuVendor('NVIDIA'), minVramGb: 16_384,
});
```

**Key facts:**
- `MeshNode` exposes `announceCapabilities`, `findNodes`, `findNodesScoped` (`net/crates/net/sdk-ts/src/mesh.ts:528-566`). Types: `net/crates/net/sdk-ts/src/capabilities.ts`.
- **No `findBestNode` in the TS SDK or NAPI binding today** (`net/crates/net/bindings/node/src/capabilities.rs` exposes only the filter path). For best-match scoring in TS, use `findNodes` + score on the caller side, or drop to Rust / Go on the placement node.
- Node ids are `bigint` — routinely exceed `Number.MAX_SAFE_INTEGER`. Don't naively `JSON.stringify`.
- `ScopeFilter` is a tagged union: `{ kind: 'any' | 'globalOnly' | 'sameSubnet' | 'tenant' | 'tenants' | 'region' | 'regions', ... }`.

### Python

```python
from net import NetMesh, normalize_gpu_vendor  # PyO3 native module

node = NetMesh("0.0.0.0:0", psk_hex)
node.start()
node.announce_capabilities({
    "hardware": {"cpu_cores": 16, "memory_gb": 64,
        "gpu": {"vendor": "nvidia", "model": "RTX 4090", "vram_gb": 24}},
    "tags": ["prod"],
    "models": [{"model_id": "llama-3.1-70b", "family": "llama", "loaded": True}],
})
peers = node.find_nodes({
    "require_gpu": True, "gpu_vendor": normalize_gpu_vendor("NVIDIA"), "min_vram_gb": 16,
})
```

**Key facts:**
- Capability surface lives on the **native** `_net.NetMesh` PyO3 class — `from net import NetMesh`. The `net_sdk.MeshNode` wrapper does not re-export these methods today; reach through `mesh._native` if you're already holding a wrapper.
- POJO shape is `dict`s with `snake_case` keys (mirrors the C/Go JSON contract). Source: `net/crates/net/bindings/python/src/capabilities.rs`.
- **No `find_best_node` in the PyO3 binding today** (parallel gap with NAPI). Use `find_nodes` and pick on the caller side, or drop to Rust / Go for scoring.
- Scope dict `kind` accepts both `snake_case` and `camelCase`: `same_subnet` and `sameSubnet` both work (`bindings/python/src/capabilities.rs:454-457`).

### Go and C

Both have the **full** surface (announce + find_nodes + find_nodes_scoped + find_best_node + find_best_node_scoped). Headers / signatures:

- **Go:** `go/capabilities.go` — `MeshNode.AnnounceCapabilities`, `FindNodes`, `FindNodesScoped`, `FindBestNode`, `FindBestNodeScoped`. `FindBestNode` returns `(uint64, bool, error)` — the bool disambiguates "no match" from `nodeId == 0` (a valid id).
- **C:** `go/net.h` — `net_mesh_announce_capabilities`, `net_mesh_find_nodes`, `net_mesh_find_nodes_scoped`, `net_mesh_find_best_node`, `net_mesh_find_best_node_scoped`. JSON in / JSON out; free returned strings via `net_free_string`.

Both also expose `net_normalize_gpu_vendor` / `NormalizeGpuVendor` (see § GPU vendor normalization below).

---

## Worked example: GPU inference routing

The full pattern — GPU node announces; requester queries; requester unicasts work.

```rust
// GPU host
use net_sdk::capabilities::{
    CapabilitySet, GpuInfo, GpuVendor, HardwareCapabilities, ModelCapability, Modality,
};
use net_sdk::mesh::MeshBuilder;

let gpu_node = MeshBuilder::new("0.0.0.0:0", &psk).build().await?;
gpu_node.announce_capabilities(
    CapabilitySet::new()
        .with_hardware(
            HardwareCapabilities::new()
                .with_memory(128)
                .with_gpu(GpuInfo::new(GpuVendor::Nvidia, "H100", 80)),
        )
        .add_model(
            ModelCapability::new("llama-3.1-70b", "llama")
                .with_context_length(131_072)
                .add_modality(Modality::Text)
                .with_loaded(true),
        )
        .add_tag("prod"),
).await?;

// Requester
use net_sdk::capabilities::{CapabilityFilter, CapabilityRequirement};

let req = CapabilityRequirement {
    filter: CapabilityFilter::new()
        .with_gpu_vendor(GpuVendor::Nvidia)
        .with_min_vram(16)
        .require_model("llama-3.1-70b")
        .require_tag("prod"),
    prefer_loaded_models: 1.0,
    prefer_more_vram: 0.3,
    ..Default::default()
};
let target = requester.find_best_node(&req).ok_or("no GPU peer matched")?;
// Open a stream / channel to `target` and send the prompt.
```

This is **Net used as a workload scheduler, not a broker.** The publisher's roster machinery never enters the picture; you picked the destination yourself by capability. Subprotocol traffic rides the same encrypted Noise session as any other mesh packet — relays forward bytes they cannot read.

---

## Scopes — capping blast radius

When you have many tenants or regions, you don't want a `find_nodes` query to span everyone. `find_nodes_scoped(filter, scope)` filters candidates through their `scope:*` reserved tags before returning.

Variants (Rust: `capability.rs:702-725`, TS: `sdk-ts/src/capabilities.ts:391-398`):

- `Any` — every peer except those tagged `scope:subnet-local` (which always require `SameSubnet`).
- `GlobalOnly` — only peers with no `scope:*` tag at all.
- `SameSubnet` — peers in the caller's subnet (caller-supplied predicate).
- `Tenant("acme")` — that tenant **plus** untagged Global peers.
- `Tenants(["acme", "beta"])` — any of those tenants **plus** Global.
- `Region("us-west")` / `Regions([...])` — same shape, region tags.

The "**plus** Global" is deliberate: untagged peers stay discoverable so a node that doesn't tag itself doesn't fall off the map by accident. To exclude untagged nodes, use `GlobalOnly` (its inverse — only untagged) or filter the result.

**Empty arrays / strings fall through to `Any`.** `Tenants([])`, `Regions([])`, `Tenant("")`, `Region("")` all resolve to `Any` rather than "match nothing" — empty scope ids would otherwise pin the query to Global-only candidates, which is rarely what the caller meant. Consistent across all SDKs.

To advertise as scoped, add the reserved tag on the announcing side. Rust: `CapabilitySet::with_tenant_scope("acme")` → `scope:tenant:acme`; `with_region_scope("us-west")` → `scope:region:us-west`; `with_subnet_local_scope()` → `scope:subnet-local`. Strictest scope wins: a peer tagged both `scope:subnet-local` and `scope:tenant:acme` resolves to `SubnetLocal`.

`scope:*` is a **discovery filter, not a routing gate.** Wire format and forwarders are unchanged. To deny packets across boundaries, see `concepts.md` § Subnets.

---

## GPU vendor normalization

```typescript
import { normalizeGpuVendor } from '@net-mesh/sdk';
normalizeGpuVendor('NVIDIA');   // 'nvidia'
normalizeGpuVendor('  AMD  ');  // 'amd' (parser is to-lower; spacing handled at filter-build time)
normalizeGpuVendor('rocm');     // 'unknown'
```

Same helper in every SDK:
- Rust: `GpuVendor::from(...)` plus `parse_gpu_vendor` in the binding source (or just use the `GpuVendor` enum directly — it's already canonical).
- Python: `from net import normalize_gpu_vendor`.
- Go: `net.NormalizeGpuVendor(s string) string`.
- C: `net_normalize_gpu_vendor(...)`.

Canonical lowercase values: `nvidia | amd | intel | apple | qualcomm | unknown`. Use this **before** building a filter so a misspelled vendor string doesn't silently collapse to `unknown` and match nothing. The on-wire `GpuVendor` enum tag is what's actually compared inside the index — the string is just the JSON-side spelling.

---

## What this is NOT

Capability routing is for **placement** (pick a node), not **delivery** (the bus still does fan-out).

- Don't build a topic system on top of `find_nodes`. Re-running the query on every emit is wasteful and the latest-announcement-wins index gives you no ordering guarantees.
- If you need ordered durable delivery to the node you picked, layer it: capability-route to find the `node_id`, then either
  - publish on a channel that node has subscribed to (`apis.md` § Named channels), or
  - open a per-peer reliable stream (`Reliability::Reliable`) and send directly.
- The capability index is **per-node, eventually consistent**. Two nodes querying at the same instant may see different candidate sets if an announcement is in flight. Don't treat results as global truth.
- Multi-hop propagation is deferred today. If your mesh has a relay-only node between announcer and querier, the querier won't see the announcement until the announcer connects directly. Plan capacity around direct-peer topology.

---

## Cross-references

- `apis.md` — general SDK shape, channel/firehose surfaces, transport selection.
- `mesh.md` — bind addresses, PSK / identity, peer setup. You need a live mesh node before any of this works.
- `concepts.md` § Subnets — how subnet policies (a *routing* gate) differ from `scope:*` tags (a *discovery* filter).
- `patterns.md` § "I want per-tenant capability discovery without standing up subnets" — the recipe-level summary that links here.
