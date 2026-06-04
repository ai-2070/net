# Capability Schema

Capabilities are the typed description of a node's properties — its hardware, its installed software, its devices, its operational role. The wire format is intentionally opaque (`tags: HashSet<Tag>` plus `metadata: BTreeMap<String, String>`), and the substrate routes whatever you put there. This page is the canonical schema that bindings codegen from: which axes exist, which keys live under each axis, and what value type each key carries.

The schema is **local** — bindings use it for auto-completion, type-checking, and runtime validation. The wire format propagates as opaque keys, so older or newer peers without the same schema see unfamiliar keys gracefully (forward-compat ride-through). Wire bumps are not schema bumps.

## Value-type vocabulary

| Type            | Wire form                                          | Example                                    |
|-----------------|----------------------------------------------------|--------------------------------------------|
| `presence`      | `<axis>.<key>` (no value)                          | `hardware.gpu`                             |
| `number`        | `<axis>.<key>=<integer>`                           | `hardware.memory_gb=64`                    |
| `string`        | `<axis>.<key>=<string>`                            | `hardware.gpu.model=H100`                  |
| `enum<T>`       | `<axis>.<key>=<value-in-T>`                        | `hardware.gpu.vendor=nvidia`               |
| `bool`          | `<axis>.<key>=true` or `=false`                    | `software.tool.0.stateless=true`           |
| `csv`           | `<axis>.<key>=v1,v2,v3`                            | `software.tool.0.requires=python:3.11,sqlite` |
| `indexed<T>`    | `<axis>.<key>.<i>.<sub>=<v>` (numeric index)      | `software.model.0.id=llama`                |
| `keyed<T>`      | `<axis>.<key>.<name>=<v>` (string key)             | `software.runtime.python=3.11`             |

Numeric fields document their integer width (u16 / u32 / u64) so cross-binding codegen produces the correct signed/unsigned shape.

## `hardware` axis

Compute properties — CPU, RAM, GPU, accelerators, storage, network, resource limits.

| Key                                          | Type            | Notes                                                              |
|----------------------------------------------|-----------------|--------------------------------------------------------------------|
| `hardware.cpu_cores`                         | `number` (u16)  | Physical cores                                                     |
| `hardware.cpu_threads`                       | `number` (u16)  | Logical threads (SMT-aware)                                        |
| `hardware.memory_gb`                         | `number` (u32)  | Total RAM in GB                                                    |
| `hardware.gpu`                               | `presence`      | Marker — primary GPU is present                                    |
| `hardware.gpu.vendor`                        | `enum<vendor>`  | `nvidia` / `amd` / `intel` / `apple` / `qualcomm` / `unknown`     |
| `hardware.gpu.model`                         | `string`        | Free-form (e.g. `RTX 4090`, `H100`, `M2 Ultra`)                    |
| `hardware.gpu.vram_gb`                       | `number` (u32)  | VRAM in GB                                                         |
| `hardware.gpu.compute_units`                 | `number` (u16)  | SM (NVIDIA) / CU (AMD) count                                       |
| `hardware.gpu.tensor_cores`                  | `number` (u16)  | Tensor / matrix engine count                                       |
| `hardware.gpu.fp16_tflops_x10`               | `number` (u32)  | FP16 TFLOPS scaled ×10 (so `825` = 82.5 TFLOPS)                    |
| `hardware.storage_gb`                        | `number` (u64)  | Persistent storage in GB                                           |
| `hardware.network_gbps`                      | `number` (u32)  | Network bandwidth in Gbps                                          |
| `hardware.limits.max_concurrent_requests`    | `number` (u32)  | Per-node concurrency cap                                           |
| `hardware.limits.max_tokens_per_request`     | `number` (u32)  | Per-request token cap                                              |
| `hardware.limits.rate_limit_rpm`             | `number` (u32)  | Requests per minute                                                |
| `hardware.limits.max_batch_size`             | `number` (u32)  | Batch dispatch cap                                                 |
| `hardware.limits.max_input_bytes`            | `number` (u32)  | Per-request input size cap                                         |
| `hardware.limits.max_output_bytes`           | `number` (u32)  | Per-request output size cap                                        |

## `software` axis

Software stack — OS, runtimes, frameworks, drivers, loaded models, tools.

| Key                                       | Type                          | Notes                                                                  |
|-------------------------------------------|-------------------------------|------------------------------------------------------------------------|
| `software.os`                             | `string`                      | Lowercase: `linux` / `darwin` / `windows`                              |
| `software.os_version`                     | `string`                      | Free-form (`6.5.0`, `14.2.1`, `11`)                                    |
| `software.cuda_version`                   | `string`                      | `<major>.<minor>` (e.g. `12.4`)                                        |
| `software.runtime.<name>`                 | `keyed<string>`               | Value is the runtime version (`software.runtime.python=3.11`)          |
| `software.framework.<name>`               | `keyed<string>`               | E.g. `software.framework.pytorch=2.1`                                  |
| `software.driver.<name>`                  | `keyed<string>`               | E.g. `software.driver.nvidia=535.86.10`                                |
| `software.model.<i>.id`                   | `indexed<string>`             | Per-model identity                                                     |
| `software.model.<i>.family`               | `indexed<string>`             | Model family (`llama`, `mistral`)                                      |
| `software.model.<i>.parameters_b_x10`     | `indexed<number>` (u32)       | Parameter count in billions, scaled ×10 (so `700` = 70.0 B)            |
| `software.model.<i>.context_length`       | `indexed<number>` (u32)       | Max context length in tokens                                           |
| `software.model.<i>.quantization`         | `indexed<string>`             | E.g. `q4_k_m`, `fp16`, `int8`                                          |
| `software.model.<i>.modalities`           | `indexed<csv<modality>>`      | CSV of `text` / `vision` / `audio` / `video` / `embeddings`            |
| `software.model.<i>.tokens_per_sec`       | `indexed<number>` (u32)       | Recent throughput                                                      |
| `software.model.<i>.loaded`               | `indexed<bool>`               | In-memory and serving                                                  |
| `software.tool.<i>.id`                    | `indexed<string>`             | Tool identifier                                                        |
| `software.tool.<i>.stateless`             | `indexed<bool>`               | Whether the tool is stateless                                          |
| `software.tool.<i>.requires`              | `indexed<csv<string>>`        | Comma-separated requirements (e.g. `python:3.11,sqlite`)               |

## `devices` axis

Physical sensors, actuators, peripherals attached to the node.

| Key                       | Type             | Notes                                          |
|---------------------------|------------------|------------------------------------------------|
| `devices.<kind>`          | `keyed<string>`  | Device kind → model (e.g. `devices.lidar=ouster-os1`) |
| `devices.camera.count`    | `number` (u8)    | Number of cameras                              |
| `devices.imu`             | `presence`       | IMU present                                    |

## `dataforts` axis

Blob storage capabilities and heat counters used by the data-gravity layer.

| Key                                   | Type            | Notes                                                      |
|---------------------------------------|-----------------|------------------------------------------------------------|
| `dataforts.cache_gb`                  | `number` (u32)  | Local blob cache size in GB                                |
| `dataforts.persistent_gb`             | `number` (u64)  | Persistent blob storage in GB                              |
| `dataforts.erasure_coded`             | `bool`          | Whether persistent tier uses Reed-Solomon encoding         |

## Reserved cross-axis prefixes

A handful of prefixes carry specific meaning to the substrate, outside the four canonical axes:

| Prefix       | Meaning                                                                |
|--------------|------------------------------------------------------------------------|
| `causal:`    | Node's position in the causal graph; also used by blob transfer (a node holding a chunk advertises `causal:<blake3-hex>`) |
| `fork-of:`   | Marks a node as a fork or replica of another entity                    |
| `heat:`      | Data-gravity counters (read frequency, write frequency)                |
| `scope:`     | Visibility scope for cross-subnet capability propagation               |
| `ai-tool:`   | Marks a node as serving the named LLM-callable tool; added automatically by `serve_tool` |
| `subprotocol:` | Subprotocols the node handles (added automatically by the registry)   |

## Reserved metadata keys

The substrate also reserves a handful of `metadata` keys for cross-cutting use cases:

| Key                       | Used by              | Purpose                                                |
|---------------------------|----------------------|--------------------------------------------------------|
| `intent`                  | Placement, RedEX     | What the node intends to do with the capability        |
| `colocate-with`           | RedEX placement      | Hint to co-locate this channel's replicas with another |
| `colocate-with-strict`    | RedEX placement      | Hard requirement: only colocate, refuse otherwise      |
| `region`                  | Subnet assignment    | Logical region label                                   |
| `tier`                    | Placement scoring    | Operational tier (`production`, `staging`, etc.)       |

## Eternal-rule alignment

Three properties hold across schema changes:

- **Wire format is unchanged.** Adding a key under an existing axis doesn't break wire compat. Older peers see opaque keys gracefully.
- **Schemas don't enforce on peer data.** A decoder sees a key it doesn't recognize; it logs a forward-compat warning and proceeds. Schemas are local validation, not protocol contracts.
- **Schema bumps are binding-version concerns.** They aren't substrate version bumps. Coordinate releases across bindings, but never break protocol compat for a schema addition.

If a future change adds a new key under an existing axis, the steps are: update this doc, regenerate the per-binding schemas, ship a coordinated release. Old peers continue to interop.
