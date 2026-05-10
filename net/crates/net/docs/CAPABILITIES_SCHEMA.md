# Capabilities Schema — canonical source of truth

> Per-axis key shape + value typing for `CapabilitySet`. **Authoritative** for the Rust core schema (`behavior::schema::AXIS_SCHEMA`) and the per-binding generated schemas (TS `.d.ts`, Python TypedDict / Pydantic, Go codegen). CI guards each binding's regenerated schema against this doc; build fails on drift.
>
> Phase 2 of [`plans/CAPABILITY_ENHANCEMENTS_PLAN.md`](plans/CAPABILITY_ENHANCEMENTS_PLAN.md). Companion to [`plans/CAPABILITY_SYSTEM_PLAN.md`](plans/CAPABILITY_SYSTEM_PLAN.md) §1 (typed taxonomy) — that doc defines the four-axis ontology; this doc enumerates the keys under each axis.

## Status

**Canonical for the Rust schema.** Bindings (TS / Python / Go) regenerate from this doc as their SDK plan Phase 9a lands; the CI guard ships when the second binding-side regenerator does.

## Frame

The substrate's wire format is `tags: HashSet<Tag>` + `metadata: BTreeMap<String, String>`. Tags are opaque strings on the wire — the substrate routes them, the codec at each end interprets them. This schema describes the **interpretation**:

- Which axes exist (`hardware`, `software`, `devices`, `dataforts` — ratified in `CAPABILITY_SYSTEM_PLAN.md` §1).
- Which keys exist under each axis.
- What value type each key carries (number, string, bool, presence, csv, indexed-collection).
- Reserved cross-axis prefixes outside the four axes (`causal:`, `fork-of:`, `heat:`, `scope:`).
- Reserved metadata keys (`intent`, `colocate-with`, tool schemas).

The schema is **purely local** — bindings use it for auto-completion, static type-checking, runtime validation. Wire format propagates as opaque tags + metadata; older / newer peers without the same schema see opaque keys gracefully (forward-compat).

## Eternal-rule alignment

Per [`plans/CAPABILITY_ENHANCEMENTS_PLAN.md`](plans/CAPABILITY_ENHANCEMENTS_PLAN.md) "Locked decisions / The eternal rule":

- This schema does NOT change wire format.
- This schema does NOT enforce on incoming peer data — forward-compat decoders pass unknown keys with a warning, not an error.
- This schema does NOT version separately from the substrate. Schema bumps are binding-version concerns, not protocol bumps.

If a future change adds a new key under an existing axis: bump this doc, regenerate the per-binding schemas, ship a coordinated release. Old peers continue to interop — they treat the new key as forward-compat ride-through.

---

## Value-type vocabulary

| Type | Wire form | Examples |
|---|---|---|
| `presence` | `<axis>.<key>` (no separator) | `hardware.gpu` |
| `number` | `<axis>.<key>=<integer>` | `hardware.memory_mb=65536` |
| `string` | `<axis>.<key>=<string>` | `hardware.gpu.model=H100` |
| `enum<T>` | `<axis>.<key>=<value>` where value ∈ T | `hardware.gpu.vendor=nvidia` |
| `bool` | `<axis>.<key>=true` or `=false` | `software.tool.0.stateless=true` |
| `csv` | `<axis>.<key>=v1,v2,v3` | `software.tool.0.requires=python:3.11,sqlite` |
| `indexed<T>` | `<axis>.<key>.<i>.<sub>=<v>` (numeric index) | `software.model.0.id=llama` |
| `keyed<T>` | `<axis>.<key>.<name>=<v>` (string key) | `software.runtime.python=3.11` |

**Value-range pinning**: numeric fields document their integer width (u16 / u32 / u64) so cross-binding codegen produces the correct signed/unsigned shape.

---

## `hardware` axis

Compute capabilities of the node — CPU, RAM, GPU, accelerators, persistent storage, network, resource limits.

| Key | Type | Range | Notes |
|---|---|---|---|
| `hardware.cpu_cores` | `number` (u16) | `1..=u16::MAX` | Physical cores. Omitted from emission when zero. |
| `hardware.cpu_threads` | `number` (u16) | `1..=u16::MAX` | Logical threads (SMT-aware). |
| `hardware.memory_mb` | `number` (u32) | `0..=u32::MAX` | Total RAM in MB. |
| `hardware.gpu` | `presence` | — | Marker that a primary GPU is present. The `hardware.gpu.*` sub-keys carry its details. |
| `hardware.gpu.vendor` | `enum<vendor>` | `nvidia` / `amd` / `intel` / `apple` / `qualcomm` / `unknown` | Lowercase. Unknown vendor values forward-compat round-trip as `Unknown`. |
| `hardware.gpu.model` | `string` | — | Free-form (`RTX 4090`, `H100`, `M2 Ultra`). |
| `hardware.gpu.vram_mb` | `number` (u32) | `0..=u32::MAX` | VRAM in MB. |
| `hardware.gpu.compute_units` | `number` (u16) | `0..=u16::MAX` | SM count (NVIDIA) / CU count (AMD). |
| `hardware.gpu.tensor_cores` | `number` (u16) | `0..=u16::MAX` | Tensor / matrix engine count. |
| `hardware.gpu.fp16_tflops_x10` | `number` (u32) | `0..=u32::MAX` | FP16 TFLOPS scaled ×10 (so `825` = 82.5 TFLOPS). u32 because aggregated cluster figures overflow u16. |
| `hardware.storage_mb` | `number` (u64) | `0..=u64::MAX` | Persistent storage in MB. |
| `hardware.network_mbps` | `number` (u32) | `0..=u32::MAX` | Network bandwidth in Mbps. |
| `hardware.limits.max_concurrent_requests` | `number` (u32) | `0..=u32::MAX` | Per-node concurrency cap. |
| `hardware.limits.max_tokens_per_request` | `number` (u32) | `0..=u32::MAX` | Per-request token cap. |
| `hardware.limits.rate_limit_rpm` | `number` (u32) | `0..=u32::MAX` | Requests per minute. |
| `hardware.limits.max_batch_size` | `number` (u32) | `0..=u32::MAX` | Batch dispatch cap. |
| `hardware.limits.max_input_bytes` | `number` (u32) | `0..=u32::MAX` | Per-request input size cap. |
| `hardware.limits.max_output_bytes` | `number` (u32) | `0..=u32::MAX` | Per-request output size cap. |

**Multi-GPU / accelerators**: deferred. The current encoding is single-primary-GPU + zero-or-more accelerators (TPU/NPU/FPGA/ASIC/DSP), but the wire encoding for accelerators ships in a follow-up. Schema additions land here when that does.

---

## `software` axis

Software stack — OS, runtimes, frameworks, drivers, loaded models, available tools.

| Key | Type | Notes |
|---|---|---|
| `software.os` | `string` | Lowercase (`linux` / `darwin` / `windows`). |
| `software.os_version` | `string` | Free-form (`6.5.0` / `14.2.1` / `11`). |
| `software.cuda_version` | `string` | `<major>.<minor>` (e.g. `12.4`). Empty string omitted. |
| `software.runtime.<name>` | `keyed<string>` | Value is the runtime version (`software.runtime.python=3.11`). Iteration order through tag set is lex-by-name. |
| `software.framework.<name>` | `keyed<string>` | E.g. `software.framework.pytorch=2.1`. |
| `software.driver.<name>` | `keyed<string>` | E.g. `software.driver.nvidia=535.86.10`. |
| `software.model.<i>.id` | `indexed<string>` | Per-model identity. `<i>` is a numeric index preserving insertion order. |
| `software.model.<i>.family` | `indexed<string>` | Model family (`llama`, `mistral`). |
| `software.model.<i>.parameters_b_x10` | `indexed<number>` (u32) | Parameter count in billions, scaled ×10 (so `700` = 70.0 B). |
| `software.model.<i>.context_length` | `indexed<number>` (u32) | Max context length in tokens. |
| `software.model.<i>.quantization` | `indexed<string>` | E.g. `q4_k_m`, `fp16`, `int8`. |
| `software.model.<i>.modalities` | `indexed<csv<modality>>` | CSV of `text` / `vision` / `audio` / `video` / `embeddings`. |
| `software.model.<i>.tokens_per_sec` | `indexed<number>` (u32) | Recent throughput. |
| `software.model.<i>.loaded` | `indexed<bool>` | Whether the model is in-memory and serving now. |
| `software.tool.<i>.tool_id` | `indexed<string>` | Per-tool identity. |
| `software.tool.<i>.name` | `indexed<string>` | Human-readable tool name. |
| `software.tool.<i>.version` | `indexed<string>` | Default `1.0.0` if unset. |
| `software.tool.<i>.requires` | `indexed<csv<string>>` | CSV of dependency strings. |
| `software.tool.<i>.estimated_time_ms` | `indexed<number>` (u32) | Typical execution time in ms. |
| `software.tool.<i>.stateless` | `indexed<bool>` | Default `true`. |

**Tool schemas (`input_schema` / `output_schema`)**: live in `metadata` (see below), not in the tag set, because JSON Schema strings can't safely round-trip through the tag wire format (they contain `=`, `:`, `,`).

---

## `devices` axis

World-facing semantic role tags — what physical / virtual devices the node represents in the operator's mental model. Currently no per-struct decoder; tags pass through as forward-compat rideshares.

Reserved key shapes (informational; bindings may emit but the substrate does not interpret):

| Pattern | Type | Example |
|---|---|---|
| `devices.<role>` | `presence` | `devices.printer`, `devices.sensor`, `devices.camera` |
| `devices.<role>.<key>=<value>` | `keyed<string>` | `devices.printer.format=pdf`, `devices.sensor.kind=temperature` |

**Validator behavior**: unknown `devices.*` keys are **forward-compat warnings**, not errors. Operators / applications that want stricter validation extend their local schema.

---

## `dataforts` axis

Storage capacity + hosted causal chains (Rebel Yell axis). Currently no per-struct decoder; tags pass through as forward-compat rideshares.

Reserved key shapes (informational):

| Pattern | Type | Example |
|---|---|---|
| `dataforts.tier` | `enum<tier>` | `hot` / `warm` / `cold` |
| `dataforts.has_chain:<hex>` | `presence` (with `:` separator) | `dataforts.has_chain:abc123` |
| `dataforts.capacity_mb` | `number` (u64) | `dataforts.capacity_mb=1048576` |

**Validator behavior**: unknown `dataforts.*` keys are forward-compat warnings.

---

## Reserved cross-axis prefixes

Per [`plans/CAPABILITY_SYSTEM_PLAN.md`](plans/CAPABILITY_SYSTEM_PLAN.md) §2: tag shapes that describe the *artifact* (chain, fork lineage, heat) rather than the node, so they don't fit a single taxonomy axis. Stored as `Tag::Reserved { prefix, body }`.

| Prefix | Body shape | Notes |
|---|---|---|
| `causal:` | `<chain_hash_hex>` | Node holds the named chain. |
| `causal:` | `<chain_hash_hex>:<tip_seq>` | Holds chain up to the named tip sequence number. |
| `causal:` | `<chain_hash_hex>[<start>..<end>]` | Holds a chain range. |
| `fork-of:` | `<parent_chain_hash_hex>` | Chain is a fork of the named parent. |
| `heat:` | `<chain_hash_hex>=<rate>` | Per-chain heat (read rate / activity score). |
| `scope:` | `tenant:<tenant_id>` | Announcement scoped to a tenant. |
| `scope:` | `region:<region_name>` | Announcement scoped to a region. |
| `scope:` | `subnet-local` | Announcement opted out of cross-subnet discovery. |
| `scope:` | `global` | Default; presence is a no-op. |

**Validator behavior**: reserved-prefix tags are recognized by their prefix; bodies are NOT type-checked beyond shape (they're application-defined). User code emitting reserved-prefix tags via `Tag::parse_user` is rejected with `CapabilityTagError::ReservedPrefix`.

---

## Metadata reserved keys

`CapabilitySet::metadata: BTreeMap<String, String>` carries free-form key-value data. The substrate-defined keys:

| Key | Value type | Notes |
|---|---|---|
| `intent` | `string` | Application-defined placement intent (e.g. `ml-training`, `embedding-cache`). Consumed by `PlacementFilter` per `CAPABILITY_SYSTEM_PLAN.md` §7. |
| `colocate-with` | `string` (chain hash hex) | Soft colocation hint — placement should prefer nodes holding this chain. |
| `colocate-with-strict` | `string` (chain hash hex) | Hard colocation requirement (placement vetoes nodes that don't hold). |
| `priority` | `string` (low/medium/high) | Application priority class. |
| `owner` | `string` | Free-form owner identity (operator-defined). |
| `tool::<id>::input_schema` | `string` (JSON Schema) | Per-tool input schema; can't ride the tag wire format. |
| `tool::<id>::output_schema` | `string` (JSON Schema) | Per-tool output schema. |

**Application-defined keys**: any `metadata` key that doesn't match one of the substrate-reserved names above. Validator passes them through unchanged; size cap (4 KB soft / 16 KB hard per `CAPABILITY_SYSTEM_PLAN.md` Locked decision 2) applies to the whole map.

**Reserved-key namespace**: `tool::*` is reserved for tool-related metadata. Other reserved namespaces may be added (`acl::*`, `provenance::*`); applications that need their own namespacing should pick names that don't collide with existing reserved patterns.

---

## Validation behavior

Each binding's `validate_capabilities(caps)` returns a `ValidationReport` with three categories:

1. **Errors** (`SchemaError`) — schema violations the operator should fix:
   - `UnknownAxis(prefix)` — a tag whose axis prefix isn't `hardware` / `software` / `devices` / `dataforts` and isn't one of the reserved prefixes (e.g. `nat:full-cone` is a `Tag::Legacy`, NOT a malformed axis tag — those legitimately ride through as untyped). This error fires only on shapes that look axis-prefixed but use an unknown axis (e.g. `compute.gpu` from a typo).
   - `TypeMismatch { key, expected, actual }` — a known key with an unparseable value (e.g. `hardware.memory_mb=lots`).
   - `ValueOutOfRange { key, value, range }` — a known numeric key whose value violates the documented range.

2. **Warnings** (`ValidationWarning`) — forward-compat or operator hygiene:
   - `UnknownKey { axis, key }` — known axis, unknown key (forward-compat: a future binding emits a key this binding doesn't yet know).
   - `MetadataOversize { soft, hard }` — `metadata` total size exceeds the soft cap.
   - `EmptyTag` — empty tag string in the set (silently dropped at parse time; surfaced here for diagnostic).

3. **Info** — purely informational, no action required:
   - `LegacyTag(string)` — a `Tag::Legacy` (untyped) tag; future major versions may deprecate.

The report's overall status:

- **Valid** — zero errors, zero warnings.
- **Valid with warnings** — zero errors, ≥ 1 warning.
- **Invalid** — ≥ 1 error.

Application code can check `report.is_valid()` before emitting / accepting a `CapabilitySet`.

---

## Cross-binding schema regeneration

Each binding's per-language schema generator reads this doc as its input. The generator does NOT parse the prose — it parses the structured tables above (`hardware` / `software` axis tables, reserved-prefix table, metadata-keys table) by their fixed column shape:

```text
| Key | Type | [Range] | Notes |
```

CI guard (illustrative; the substrate-side generator examples below are not yet implemented — see `plans/CAPABILITY_SYSTEM_SDK_PLAN.md` Phase 9a):

```bash
# After Phase 9a generators land — regenerate each binding's schema
# and fail if it differs from the committed copy.
cargo run --example gen_schema_rust > /tmp/rust_schema.rs.gen
diff -u net/crates/net/src/adapter/net/behavior/schema_generated.rs /tmp/rust_schema.rs.gen
# Same for TS / Python / Go.
```

The `examples/gen_schema_*` binaries do not exist in the substrate today; running the snippet above will fail with `no example target named ...`. Until Phase 9a lands, the guard runs as a no-op — bindings whose generators haven't shipped yet are exempted.

---

## See also

- [`plans/CAPABILITY_SYSTEM_PLAN.md`](plans/CAPABILITY_SYSTEM_PLAN.md) — §1 ratifies the four-axis ontology this schema enumerates.
- [`plans/CAPABILITY_ENHANCEMENTS_PLAN.md`](plans/CAPABILITY_ENHANCEMENTS_PLAN.md) Phase 2 — the schema layer this doc anchors.
- [`plans/CAPABILITY_SYSTEM_SDK_PLAN.md`](plans/CAPABILITY_SYSTEM_SDK_PLAN.md) §10 + Phase 9a — per-binding generators that read this doc.
- [`BEHAVIOR.md`](BEHAVIOR.md) — overall behavior plane this lives in.
