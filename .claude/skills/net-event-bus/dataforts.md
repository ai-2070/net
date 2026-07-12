# Dataforts — greedy cache, gravity, blob refs, read-your-writes

Dataforts is the **compositional data plane** on top of the substrate. Four phases compose against the existing RedEX / CortEX / capability-index / proximity-graph layers — there's no new wire protocol and no separate coordinator service. All four ship behind the single `dataforts` Cargo feature; pre-built artifacts ship with it enabled.

Greedy and gravity are **runtime-toggleable policies** — operators flip them on / off live against a running mesh, no rebuild required. The Cargo feature gates whether the surface compiles at all; the per-phase decision is operational.

If you came here from `redex.md` or `cortex.md` looking for "how do I read my own write deterministically?" — that's Phase 5 below. If you came looking for "how do I cache chains from peers automatically?" — that's Phase 1. The four phases are independent; pick the ones that match your workload.

---

## When to reach for Dataforts

| Need | Phase | Lookup |
|---|---|---|
| "Nodes near a chain should cache it speculatively, evict cold ones under pressure" | Phase 1 — Greedy | `Redex::enable_greedy_dataforts(mesh, …)` |
| "Hot chains should drift toward the readers that drive the heat" | Phase 4 — Gravity | `Redex::enable_gravity_for_greedy(mesh, …)` |
| "Substrate should carry a content-addressed reference; bytes live in S3 / Ceph / IPFS / local FS" | Phase 3 — Blob | `BlobAdapterRegistry::register` + `blob_publish` / `blob_resolve` |
| "Move a blob / directory **peer-to-peer over the mesh** — no external store in the path" | Transfer | `serve_blob_transfer` + `fetch_blob` / `store_dir` / `fetch_dir` |
| "Producer needs to read its own write deterministically through the cache" | Phase 5 — RYW | `tasks.wait_for_token(token, deadline)` |

The four phases are independent. A deployment can run greedy without gravity (hoard, don't rebalance), gravity without greedy (drift-only on pre-seeded replicas), both, or neither (substrate-only). Same for blob and RYW.

---

## Phase 1 — Greedy-LRU caching

Per-node speculative caching of in-scope chains observed via the tail-subscription path. The mesh fans every event through a `GreedyObserver`; the runtime decides whether to admit each event into a per-channel cache file. Cold channels evict under cluster-cap pressure and withdraw their `causal:<hex>` advertisement so peers re-route to a healthy holder.

### `GreedyConfig` — the knobs

| Field | Default | What it does |
|---|---|---|
| `scopes` | `[]` | Scope-label filter (e.g. `region:us`, `env:prod`). Only inbound events whose chain caps include a matching scope tag are admission-eligible. Empty = no scope filter. |
| `proximity_max_rtt` | `200 ms` | Proximity-axis gate — events from peers with RTT > this aren't admission-eligible. It's a plain `Duration`, not an `Option`; a **zero** value is *rejected* by config validation (`greedy proximity_max_rtt must be non-zero`), not a way to disable the gate. |
| `per_channel_cap_bytes` | `100 MiB` | Per-channel storage cap. Events that would push a channel over this size evict-then-admit (LRU within the channel). |
| `total_cap_bytes` | `10 GiB` | Cluster-cap — total bytes across all cached channels. Triggers cluster-level eviction (cold channels evict whole). |
| `bandwidth_budget_fraction` | `0.25` | Share of measured NIC peak the cache is allowed to consume. Token-bucket gated; over-budget admits bump a counter and reject the event. |
| `nic_peak_bytes_per_s` | `None` (→ 125 MB/s = 1 Gbps) | Operator override of the NIC-peak probe. Set explicitly on > 1 Gbps NICs. |
| `intent_match` | `AnyOfLocalCapabilities` | Capability-preference axis. `IntentMatchPolicy` is `Disabled` \| `AnyOfLocalCapabilities` \| `Strict`; `GreedyConfig::default()` sets `AnyOfLocalCapabilities` (admit chains whose `intent:<label>` the local node has capability for). `Strict` requires the registry's declared capabilities; `Disabled` passes the axis. There is no `MatchAnyAdvertised`. |
| `colocation_policy` | `SoftPreference` | Colocation axis. `ColocationPolicy` is `Ignore` \| `SoftPreference` \| `StrictRequired`; `GreedyConfig::default()` sets `SoftPreference` (raises admission preference). `StrictRequired` rejects events whose colocation target isn't already cached locally; `Ignore` disables the axis. |
| `observer_inflight_cap` | `1024` | `tokio::sync::Semaphore` size on the observe fan-out. Saturation drops events and bumps `dataforts_greedy_observer_dropped_overloaded`. |

### Wire-up

```rust
use net::adapter::net::{Redex, MeshNode};
use net::adapter::net::dataforts::{GreedyConfig, IntentMatchPolicy};
use net::adapter::net::behavior::capability::CapabilitySet;
use std::sync::Arc;

let redex = Arc::new(Redex::new());

redex.enable_greedy_dataforts(
    mesh.clone(),
    GreedyConfig::new()
        .with_scopes(vec!["region:us".into()])
        .with_total_cap_bytes(1 << 30)   // 1 GiB
        .with_per_channel_cap_bytes(64 << 20)
        .with_intent_match(IntentMatchPolicy::Disabled),
    Arc::new(CapabilitySet::default()),
    Default::default(),    // IntentRegistry::new()
)?;

// Cache lookup — Some(RedexFile) when greedy admitted this chain,
// None when it didn't (caller falls back to network fetch).
let file = redex.greedy_cache_for(&channel_name);
```

```python
redex.enable_greedy_dataforts(
    mesh,
    scopes=['region:us'],
    total_cap_bytes=1 << 30,
    per_channel_cap_bytes=64 << 20,
)
```

```ts
redex.enableGreedyDataforts(mesh, {
  scopes: ['region:us'],
  totalCapBytes: 1n << 30n,
  perChannelCapBytes: 64n << 20n,
});
```

### Operational notes

- **Bandwidth-budget rejection has its own counter** (`dataforts_greedy_admit_throttled_bandwidth_total`). Disambiguate "NIC saturated" from "cache full" on the operator dashboard.
- **Cluster-cap eviction withdraws chain announcements inline.** Peers see the `causal:<hex>` advertisement drop in the same tick.
- **`upsert` on reopen subtracts old bytes from `total_bytes`.** The cluster-cap budget stays accurate across reopens.
- **`observer_inflight_cap` is the spawn fan-out bound.** A flooding peer can't pile up unbounded outstanding tasks; on saturation the event drops and the counter bumps.
- **`disable_greedy_dataforts()` removes the observer.** The runtime drops the cache files (in-memory) or leaves them on disk (persistent) — disable is a runtime decision, not a cleanup signal.
- **Cache files are keyed by wire `u16`**, not canonical `u32` — the cache name is `dataforts/greedy/<hex16>`. Two wire-colliding channels share a cache file (a small mix-up at the data-plane layer); ACL / config / RYW decisions stay collision-safe via the canonical hash.

---

## Phase 4 — Data gravity

Per-chain read-rate counters with exponential decay. Threshold-crossing emissions stamp `heat:<hex>=<rate>` onto the chain's existing capability announcement; greedy admission weights cache pulls by `heat × scope-match × proximity-rank`. Cold chains evict first under cluster-cap pressure; hot chains migrate toward the readers that drive the heat. Gravity emerges from greedy + heat counters + capability-preference automatically — no separate migration engine.

### `DataGravityPolicy` — the knobs

| Field | Default | What it does |
|---|---|---|
| `enabled` | `true` | Master switch (gravity can be enabled-but-quiescent). |
| `emit_threshold_ratio` | `2.0` | Emit a new `heat:` tag when the current rate exceeds `prev × ratio` OR drops below `prev / ratio`. Higher = quieter wire traffic, lower = more responsive heat tracking. Range `[1.01, 10.0]`. |
| `decay_half_life` | `30 min` | Exponential-decay half-life. A chain read once and then ignored for 30 min drops to half-rate; after 60 min, quarter-rate. |
| `normalization_reference_rate` | `1000.0` events/s | Maps to 1.0 on the wire. `ln_1p(rate) / ln_1p(reference)`; a 1000/s chain emits `heat:<hex>=1.00`. Range minimum `1.5`. |

### Wire-up

```rust
use net::adapter::net::dataforts::DataGravityPolicy;
use std::time::Duration;

redex.enable_gravity_for_greedy(
    mesh.clone(),
    DataGravityPolicy::new()
        .with_emit_threshold_ratio(1.5)
        .with_decay_half_life(Duration::from_secs(300))
        .with_normalization_reference_rate(500.0),
)?;
```

```python
redex.enable_gravity_for_greedy(
    mesh,
    emit_threshold_ratio=1.5,
    decay_half_life_secs=300,
    normalization_reference_rate=500.0,
)
```

```ts
redex.enableGravityForGreedy(mesh, {
  enabled: true,
  emitThresholdRatio: 1.5,
  decayHalfLifeSecs: 300n,
  normalizationReferenceRate: 500.0,
});
```

### Operational notes

- **Requires greedy first.** `enable_gravity_for_greedy` without a prior `enable_greedy_dataforts` returns `RedexError`.
- **Heat tags are auth-gated on the publisher's `causal:` claim.** A peer advertising `heat:X` without simultaneously advertising `causal:X` has its heat tag dropped at the receive boundary. Per-peer rate-limit of heat emissions is not yet implemented.
- **`HeatRegistry` is capped at 8 K entries with LRU eviction by `last_update`.** Misbehaving peers can't flood the registry past the cap.
- **`should_emit_heat` is subnormal-safe.** Near-zero `prev` (1e-300, subnormals) doesn't trip `inf`-prone ratio arithmetic; NaN rates return `Skip`.
- **Log-scale wire normalization.** The wire uses `ln_1p(rate) / ln_1p(reference)` with the configurable reference rate, so warm chains and blazing chains stay distinguishable rather than compressing asymptotically.
- **`gravity_tick` is batched.** All chain emissions in a tick coalesce into one `announce_heat_batch` + one `announce_capabilities` rewrite — not O(n²) on a 100 K-chain node.
- **`origin_hash == 0` skip.** Default-constructed publishers carry `origin_hash = 0`; gravity skips heat bumps on unattributed origins as defense-in-depth.

---

## Phase 3 — `BlobRef` + `BlobAdapter`

Content-addressed reference whose bytes live in the caller's existing storage (S3, Ceph, IPFS, local FS). The substrate carries the reference, never owns the bytes. Adapters implement `fetch` / `store` (or the streaming variants for multi-GB payloads); the `FileSystemAdapter` ships in-tree.

### Wire format

```text
[0xB0, 0xB1, 0xB2, 0xB3]  // 4-byte magic
version: u8               // currently 1
hash:    [u8; 32]         // BLAKE3
size:    u64              // bytes; bounded by BLOB_REF_MAX_SIZE = 16 GiB
uri:     [u8]             // length-prefixed; adapter dispatch key
```

Adapter dispatch is **URI-scheme keyed**, not channel-config keyed. `BlobAdapter::accepted_schemes() -> &[&str]` declares which URI schemes an adapter handles (`["s3", "s3+https"]`, `["file"]`, etc.); the registry routes by scheme. Because the channel config does not select the adapter, an attacker who can write to a channel can't route their `BlobRef` URI through an arbitrary registered adapter.

### Operational notes

- **Hash-verify on store.** `FileSystemAdapter::store(blob_ref, &bytes)` BLAKE3-hashes the supplied bytes and rejects mismatch.
- **`fsync` of temp + parent dir** lands in the FS store path. Power loss between rename and OS flush doesn't leave zero-length files in the addressable space.
- **Unique tmp suffixes.** `<hash>.<pid>.<atomic>.<nanos>.tmp` — concurrent stores on the same hash don't race or fail on Windows-rename, and idempotent re-stores hash-verify.
- **Streaming hooks.** `fetch_stream` / `store_stream` ship as required methods on `BlobAdapter` with default implementations that route through `fetch` / `store`; adapters wanting real streaming override. FS adapter chunks at 256 KiB.
- **`BlobRef::MAX_SIZE = 16 GiB` default cap.** Decode rejects larger sizes; `RedexFileConfig::with_blob_max_size` lifts the cap when an operator needs it.
- **Per-channel registry override.** `RedexFileConfig::with_blob_adapter_registry(Some(arc))` for multi-tenant isolation; default-tenant path uses the global singleton.
- **`BlobError::NotFound(uri)` sanitizes the URI.** Control chars escape as `\xNN`, length caps at 256 bytes — a binding logging the error can't be log-injected by an attacker who controls the URI.

### Wire-up

```rust
use net::adapter::net::dataforts::{FileSystemAdapter, publish_blob, resolve_payload};

// The FS adapter accepts only `file:` URIs (accepted_schemes() == ["file"]);
// publish enforces that gate, so a `local://` URI errors with UnsupportedScheme.
let adapter = FileSystemAdapter::new("fs", "/var/blobs");
let encoded = publish_blob(&adapter, "file:obj/payload-1", &large_payload).await?;
// `encoded` is the addressable reference bytes that ride events as the payload.

let payload = resolve_payload(&encoded, &adapter).await?;
```

```python
from net import register_filesystem_blob_adapter, blob_publish, blob_resolve

register_filesystem_blob_adapter('local', '/var/blobs')
blob_ref = blob_publish('local', 'file:obj/payload-1', large_payload)   # FS adapter accepts only file: URIs
payload = blob_resolve(blob_ref)
```

```ts
import { registerFilesystemBlobAdapter, blobPublish, blobResolve } from '@net-mesh/core';

registerFilesystemBlobAdapter('local', '/var/blobs');
const blobRef = await blobPublish('local', 'file:obj/payload-1', largePayload);  // FS adapter accepts only file: URIs
const payload = await blobResolve(blobRef);
```

### Custom adapters

Each binding lets you write adapters in the host language:

- **Python** — `register_blob_adapter(id, instance)` where `instance` implements `fetch` / `store` (sync or `async def`). Async adapters run on a binding-owned event loop on a dedicated thread (no fresh `asyncio.run` per call). An `aiobotocore` / `httpx.AsyncClient` / SQLAlchemy async engine inside the adapter is safe.
- **Node** — `registerBlobAdapter(id, instance)` (sync TSFN bridge) or `registerAsyncBlobAdapter(id, instance)` (Promise-returning TSFN bridge).
- **C / cgo** — `NetBlobAdapterVtable` with per-field null-check at registration; partial vtables return `NET_ERR_BLOB_VTABLE_INVALID`.

---

## Mesh blob + directory transfer

Phase 3's `BlobRef` is a *reference*; the bytes still have to move. Besides the storage adapters (S3 / Ceph / FS pull), there's **peer-to-peer transfer over the mesh transport itself** — `SUBPROTOCOL_BLOB_TRANSFER` rides the fair-scheduled streams (`StreamConfig.scheduled = true`, see `streams.md`), so a node fetches a content-addressed blob — or a whole directory — directly from a peer that holds it, with no external store in the path.

This composes with Phase 3: `store_dir` writes chunks **into** a `BlobAdapter` and returns a manifest `BlobRef`; `fetch_blob` / `fetch_dir` pull those chunks **over the mesh** from whoever holds them. You can use the transfer surface without an S3-style backend at all (the FS adapter is enough).

**How it works.** Discovery rides the capability fold's `causal:<hex>` advertisement. The requester picks a holder, opens a freshly-allocated transfer stream, and the holder validates possession-of-hash as the capability, then chunks the blob into ≤8108-byte reliable events terminated by FIN. The receiver concatenates by arrival order and verifies BLAKE3. **The hash is an unguessable 256-bit bearer token** — anyone who can name the hash can fetch the bytes, so sensitive content must layer channel / capability auth above this transport (or treat the hash itself as a secret).

**Sizes + atomicity.** Default chunk `BLOB_CHUNK_SIZE_BYTES = 4 MiB`; per-chunk ceiling `TRANSFER_MAX_CHUNK_BYTES = 16 MiB`; wire events ≤8108 bytes. Both send and receive stream chunk-at-a-time (peak memory ≈ one chunk), so total transfer size is **disk-bound, not memory-bound**. `fetch_dir` is **atomic** — it writes to a sibling temp path and renames, so the destination either becomes the complete tree or stays exactly as it was; a mid-fetch failure is a complete rollback. Manifest paths are validated to stay within `dest`, so a hostile sender can't escape the destination root.

**Install first.** A node must call `serve_blob_transfer(mesh, adapter)` before it can serve chunks to peers **or** issue its own fetches (`fetch_blob` registers state on the local engine). `serve_blob_transfer_rpc` additionally registers the `blob.transfers` operator RPC so `net-mesh transfer ls / status / cancel` can introspect this node's in-flight requester-side transfers.

### Surface (verified against `sdk/src/transport.rs` + bindings)

| Op | Rust (`net_sdk::transport`) | Node (mesh method) | Python (`_net` fn) |
|---|---|---|---|
| install engine | `serve_blob_transfer(&mesh, adapter)` | `mesh.serveBlobTransfer(adapter)` | `serve_blob_transfer(mesh, adapter)` |
| fetch a blob (known holder) | `fetch_blob(&mesh, source, &ref) -> Bytes` | `mesh.fetchBlob(holderId, ref) -> Buffer` | `fetch_blob(mesh, holder_id, ref) -> bytes` |
| fetch, discover holder | `fetch_blob_discovered(&mesh, &ref)` | `mesh.fetchBlobDiscovered(ref)` | `fetch_blob_discovered(mesh, ref)` |
| stream a blob chunk-wise | `fetch_blob_stream(&mesh, source, &ref)` | — | — |
| store a dir → manifest ref | `store_dir(adapter, &root) -> BlobRef` | `mesh.storeDir(adapter, root) -> BlobRef` | `store_dir(mesh, adapter, root) -> BlobRef` |
| fetch a dir | `fetch_dir(&mesh, source, &ref, dest, concurrency) -> DirStats` | `mesh.fetchDir(sourceId, ref, dest) -> {files, bytes}` | `fetch_dir(mesh, source_id, ref, dest) -> (files, bytes)` |

`DirStats { files: usize, bytes: u64 }`. `concurrency = 0` → `DEFAULT_FETCH_CONCURRENCY = 16` leaf files in flight. `BlobRef::Small` is one chunk; `BlobRef::Manifest` is its ordered chunk list; **`BlobRef::Tree` is not supported by the transport wrappers** (use the substrate tree walk). The SDK stays thin — no retry policy, no rollback machinery beyond `fetch_dir`'s atomic rename, no directory-sync primitives; applications compose policy above.

**Go / C:** the FFI symbols ship in `src/ffi/transport.rs` (`net_serve_blob_transfer`, `net_fetch_blob`, `net_fetch_blob_discovered`, `net_store_dir`, `net_fetch_dir`, `net_dir_manifest_read`) and Go binds them over cgo. Note: they are **not declared in `include/net.h`** — a C consumer declares the prototypes against the exported symbols directly. The ergonomic wrappers are Rust / Node / Python, with the C ABI present but the header not yet regenerated.

### Rust

```rust
use net_sdk::transport::{serve_blob_transfer, fetch_blob, store_dir, fetch_dir};
use std::{path::Path, sync::Arc};

// One-time: install the engine over a blob adapter (FS / S3 / …).
serve_blob_transfer(&mesh, Arc::new(adapter));

// Producer: hash a directory into content-addressed blobs, get the root ref.
let manifest_ref = store_dir(&fs_adapter, Path::new("/data/model")).await?;
// …publish `manifest_ref` on a channel so a consumer learns the token…

// Consumer: pull one blob from a known holder, BLAKE3-verified.
let bytes = fetch_blob(&mesh, holder_node_id, &blob_ref).await?;

// Consumer: materialize a whole directory atomically (0 = default concurrency).
let stats = fetch_dir(&mesh, holder_node_id, &manifest_ref, Path::new("/dest"), 0).await?;
println!("{} files, {} bytes", stats.files, stats.bytes);
```

```ts
mesh.serveBlobTransfer(adapter);
const manifestRef = await mesh.storeDir(adapter, "/data/model");
const bytes = await mesh.fetchBlob(holderNodeId, blobRef);          // Buffer
const { files, bytes: n } = await mesh.fetchDir(sourceId, manifestRef, "/dest");
```

```python
_net.serve_blob_transfer(mesh, adapter)
manifest_ref = _net.store_dir(mesh, adapter, "/data/model")
data = _net.fetch_blob(mesh, holder_id, blob_ref)                   # bytes
files, n = _net.fetch_dir(mesh, source_id, manifest_ref, "/dest")
```

### Operator CLI — `net-mesh transfer`

When a `MeshNode` is reachable through the standard `CliContext`, the operator CLI (`net-mesh` binary) moves blobs without writing code:

| Command | Does |
|---|---|
| `net-mesh transfer send-blob <path> [--store]` | chunk a file (or stdin via `-`), optionally persist each chunk, print the `BlobRef` hex |
| `net-mesh transfer recv-blob <source> <ref> --out <path>` | fetch one blob from a peer, stream to disk (temp-and-rename) |
| `net-mesh transfer send-dir <path>` | walk + hash a directory, print the root manifest `BlobRef` hex |
| `net-mesh transfer recv-dir <source> <root-ref> --dest <path>` | materialize a directory tree **atomically** |
| `net-mesh transfer ls` / `status <id>` / `cancel <id>` | list / inspect / abort in-flight transfers |

The verbs compose with the shell (pipe into `send-blob`, redirect `recv-blob` to stdout) and render a determinate byte-progress bar for sized fetches. They ship behind the `cli` feature flag. **Full flag surface, atomic-write / exit-code semantics, and scripting notes: `cli.md`.**

---

## Phase 5 — Read-your-writes

Every successful `Tasks::create` / `Memories::insert` / etc. returns the RedEX **seq** (a `u64`). The simplest read-your-writes wait is `wait_for_seq(seq)` — it blocks until the local fold has actually *applied* that sequence number, not just folded it. When you need a deadline or the origin-bound token primitive, wrap the seq in a `WriteToken { origin_hash, seq }` and call `wait_for_token(token, deadline)`. Either way, a producer reads its own write through the cache deterministically; no busy-poll, no time-window heuristic.

This piece composes with `cortex.md` — the WriteToken is what flows out of every CortEX write; the wait_for_token call is what reads block on.

### `WriteToken` shape

```rust
pub struct WriteToken {
    pub origin_hash: u64,
    pub seq: u64,
}
```

Fields are `pub` — build one with a struct literal, or `WriteToken::new(origin_hash, seq)` (a `#[doc(hidden)]` constructor). `impl FromStr` is available unconditionally; there is no `version` field. **Tokens are unforgeable only against the adapter that issued them** — origin-bound. A token claiming `origin_hash = X` passed to an adapter whose `origin_hash = Y` rejects with `WaitForTokenError::WrongOrigin`.

### Wire-up

```rust
let tasks = Tasks::open(redex, channel, origin_hash, cfg)?;
let seq = tasks.create(1, "first", now_ns)?;      // now_ns: u64 wall-clock nanoseconds
let _ = tasks.wait_for_seq(seq).await;            // block until the fold applied it
// State now reflects the create — read tasks.state() safely.
// For a deadline, wrap the seq: wait_for_token(WriteToken { origin_hash, seq }, dur).
```

```python
seq = tasks.create(1, 'first', now_ns())
tasks.wait_for_seq(seq)
# For a deadline: tasks.wait_for_token(token, deadline_ms=250)  (deadline_ms=0 = non-blocking poll)
```

```ts
const seq = tasks.create(1n, 'first', BigInt(now()));
await tasks.waitForSeq(seq);
// For a deadline: await tasks.waitForToken(token, 250);  (deadlineMs === 0 = non-blocking poll)
```

```go
seq, _ := tasks.Create(1, "first", uint64(time.Now().UnixNano()))
if err := tasks.WaitForSeq(seq, 250*time.Millisecond); err != nil { /* … */ }
// Go's RYW is seq-based — the binding has no WriteToken type or WaitForToken.
```

### Operational notes

- **`applied_through_seq()` vs. `folded_through_seq()`.** The wait keys on **applied** (events that actually ran through the fold), not the *folded* watermark — so a producer whose write hit a `FoldErrorPolicy::Skip` (via `RedexError::is_recoverable_decode`) doesn't get a premature `Ok(())` over state that doesn't reflect its write.
- **`FoldStopped` is a real error.** When `running == false` (fold task crashed under `FoldErrorPolicy::Stop`), the wait surfaces `WaitForTokenError::FoldStopped { applied_through_seq }` rather than resolving every pending RYW wait with a silent `Ok(())`.
- **`deadline_ms == 0` is a non-blocking poll** across every binding. Synchronous applied-vs-token check; no wait scheduled.
- **Process-wide in-flight cap.** `set_global_ryw_inflight_cap(usize)` sets a process-wide bound on outstanding RYW waiters; every `wait_for_token` does a two-tier acquire (process-wide then per-adapter). The default per-adapter cap is 1024 (`ryw_inflight_cap`, non-FIFO).
- **Go's RYW surface is seq-based.** `WaitForSeq(seq, timeout)` is the only wait — the Go binding exposes no `WriteToken` type and no context-aware variant, so there's no cancellation knob beyond the `timeout` argument.

---

## Common gotchas

- **`dataforts` feature must be on.** Builds without it surface typed `RedexError` stubs from every `enable_*` entry point: `"requires the 'dataforts' feature; rebuild with --features dataforts"`. Pre-built artifacts ship with the feature enabled.
- **Greedy admission rejection has five reasons** (`AdmitRejectReason::{Scope, Intent, Colocation, Capacity, Bandwidth}`) — there is no `Proximity` variant. Each has its own Prometheus counter — disambiguate "why isn't this chain being cached?" by checking which counter bumped.
- **Gravity without greedy is allowed.** A node with `enable_gravity_for_greedy` but no `enable_greedy_dataforts` is the "drift-only" quadrant — already-placed replicas emit heat, but the node doesn't speculatively cache.
- **`Redex::greedy_cache_for(channel) -> Option<RedexFile>`** returns the cache file if greedy admitted that chain; the caller falls back to a network fetch / substrate read path on `None`. The substrate doesn't auto-route reads through the cache — it's an explicit lookup.
- **Blob refs aren't transactionally tied to the bus.** A `BlobRef` riding on a published event references bytes that the adapter must have stored *before* the event was published; if the consumer reads the event before the adapter persists the bytes, `blob_resolve` fails until the persist completes.
- **`WriteToken` must come from the same adapter you wait on.** Cross-adapter tokens fail with `WrongOrigin`. The token isn't a generic "future state" handle — it's bound to one adapter's fold.

---

## When you need more

- **Full plan + activation gates per phase**: `net/crates/net/docs/misc/DATAFORTS_PLAN.md`.
- **Wishlist audit** (what's a Dataforts phase vs. what already ships via core primitives): `net/crates/net/docs/misc/DATAFORTS_FEATURES.md`.
- **Cargo feature interaction**: `net` crate's `dataforts` feature pulls `cortex + blake3 + xxhash-rust`. Builds without it get the substrate path unchanged (RedEX, CortEX, NetDB, replication all work as normal).
