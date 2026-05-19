# Dataforts — greedy cache, gravity, blob refs, read-your-writes

Dataforts is the **compositional data plane** that lands in v0.15 ("Rebel Yell") on top of the v0.14 substrate. Four phases compose against the existing RedEX / CortEX / capability-index / proximity-graph layers — there's no new wire protocol and no separate coordinator service. All four ship behind the single `dataforts` Cargo feature; pre-built release artifacts ship with it enabled.

Greedy and gravity are **runtime-toggleable policies** — operators flip them on / off live against a running mesh, no rebuild required. The Cargo feature gates whether the surface compiles at all; the per-phase decision is operational.

If you came here from `redex.md` or `cortex.md` looking for "how do I read my own write deterministically?" — that's Phase 5 below. If you came looking for "how do I cache chains from peers automatically?" — that's Phase 1. The four phases are independent; pick the ones that match your workload.

---

## When to reach for Dataforts

| Need | Phase | Lookup |
|---|---|---|
| "Nodes near a chain should cache it speculatively, evict cold ones under pressure" | Phase 1 — Greedy | `Redex::enable_greedy_dataforts(mesh, …)` |
| "Hot chains should drift toward the readers that drive the heat" | Phase 4 — Gravity | `Redex::enable_gravity_for_greedy(mesh, …)` |
| "Substrate should carry a content-addressed reference; bytes live in S3 / Ceph / IPFS / local FS" | Phase 3 — Blob | `BlobAdapterRegistry::register` + `blob_publish` / `blob_resolve` |
| "Producer needs to read its own write deterministically through the cache" | Phase 5 — RYW | `tasks.wait_for_token(token, deadline)` |

The four phases are independent. A deployment can run greedy without gravity (hoard, don't rebalance), gravity without greedy (drift-only on pre-seeded replicas), both, or neither (substrate-only). Same for blob and RYW.

---

## Phase 1 — Greedy-LRU caching

Per-node speculative caching of in-scope chains observed via the tail-subscription path. The mesh fans every event through a `GreedyObserver`; the runtime decides whether to admit each event into a per-channel cache file. Cold channels evict under cluster-cap pressure and withdraw their `causal:<hex>` advertisement so peers re-route to a healthy holder.

### `GreedyConfig` — the knobs

| Field | Default | What it does |
|---|---|---|
| `scopes` | `[]` | Scope-label filter (e.g. `region:us`, `env:prod`). Only inbound events whose chain caps include a matching scope tag are admission-eligible. Empty = no scope filter. |
| `proximity_max_rtt` | `200 ms` | Proximity-axis gate — events from peers with RTT > this aren't admission-eligible. `None` skips the gate. |
| `per_channel_cap_bytes` | `100 MiB` | Per-channel storage cap. Events that would push a channel over this size evict-then-admit (LRU within the channel). |
| `total_cap_bytes` | `10 GiB` | Cluster-cap — total bytes across all cached channels. Triggers cluster-level eviction (cold channels evict whole). |
| `bandwidth_budget_fraction` | `0.25` | Share of measured NIC peak the cache is allowed to consume. Token-bucket gated; over-budget admits bump a counter and reject the event. |
| `nic_peak_bytes_per_s` | `None` (→ 125 MB/s = 1 Gbps) | Operator override of the NIC-peak probe. Set explicitly on > 1 Gbps NICs. |
| `intent_match` | `Disabled` | Capability-preference axis. `MatchAnyAdvertised` only admits chains whose `intent:<label>` is in the local node's advertised intent set. |
| `colocation_policy` | `Ignore` | Colocation axis. `SoftPreference` raises admission preference; `StrictRequired` rejects events whose colocation target isn't already cached locally. |
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
- **Cluster-cap eviction withdraws chain announcements inline.** Peers see the `causal:<hex>` advertisement drop in the same tick (D-1 fix).
- **`upsert` on reopen subtracts old bytes from `total_bytes`.** Pre-v0.15 the cluster-cap budget drifted upward over reopens until admission rejected everything (D-3 fix).
- **`observer_inflight_cap` is the spawn fan-out bound.** A flooding peer can't pile up unbounded outstanding tasks; on saturation the event drops and the counter bumps (D-7 fix).
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
- **Heat tags are auth-gated on the publisher's `causal:` claim.** A peer advertising `heat:X` without simultaneously advertising `causal:X` has its heat tag dropped at the receive boundary (D-11). Per-peer rate-limit of heat emissions is acknowledged as deferred (N-8).
- **`HeatRegistry` is capped at 8 K entries with LRU eviction by `last_update`.** Misbehaving peers can't flood the registry past the cap (D-10, N-2 fix).
- **`should_emit_heat` is subnormal-safe.** Near-zero `prev` (1e-300, subnormals) no longer trips `inf`-prone ratio arithmetic (D-29 / N-9 fix); NaN rates return `Skip`.
- **Log-scale wire normalization.** Pre-v0.15 the wire used `(rate / (rate + 1))` which compressed asymptotically — every "warm" chain looked like "blazing." v0.15 uses `ln_1p(rate) / ln_1p(reference)` with the configurable reference rate (D-30 fix).
- **`gravity_tick` is batched.** All chain emissions in a tick coalesce into one `announce_heat_batch` + one `announce_capabilities` rewrite (D-25 fix). Pre-fix it was O(n²) on a 100 K-chain node.
- **`origin_hash == 0` skip.** Default-constructed publishers carried `origin_hash = 0`; gravity now skips heat bumps on unattributed origins as defense-in-depth (D-9 fix).

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

Adapter dispatch is **URI-scheme keyed**, not channel-config keyed. `BlobAdapter::accepted_schemes() -> &[&str]` declares which URI schemes an adapter handles (`["s3", "s3+https"]`, `["file"]`, etc.); the registry routes by scheme. Pre-v0.15 the channel config selected the adapter — an attacker who could write to a channel could route their `BlobRef` URI through any registered adapter (D-13 authority-confusion fix).

### Operational notes

- **Hash-verify on store.** `FileSystemAdapter::store(blob_ref, &bytes)` BLAKE3-hashes the supplied bytes and rejects mismatch (D-12).
- **`fsync` of temp + parent dir** lands in the FS store path. Power loss between rename and OS flush no longer leaves zero-length files in the addressable space (D-33).
- **Unique tmp suffixes.** `<hash>.<pid>.<atomic>.<nanos>.tmp` — concurrent stores on the same hash no longer race or fail on Windows-rename (D-32, N-6 hash-verify on idempotent re-store).
- **Streaming hooks.** `fetch_stream` / `store_stream` ship as required methods on `BlobAdapter` with default implementations that route through `fetch` / `store` for back-compat; adapters wanting real streaming override (D-16). FS adapter chunks at 256 KiB.
- **`BlobRef::MAX_SIZE = 16 GiB` default cap.** Decode rejects larger sizes; `RedexFileConfig::with_blob_max_size` lifts the cap when an operator needs it (D-15).
- **Per-channel registry override.** `RedexFileConfig::with_blob_adapter_registry(Some(arc))` for multi-tenant isolation; default-tenant path uses the global singleton unchanged (D-34).
- **`BlobError::NotFound(uri)` sanitizes the URI.** Control chars escape as `\xNN`, length caps at 256 bytes — a binding logging the error can't be log-injected by an attacker who controls the URI (D-31).

### Wire-up

```rust
use net::adapter::net::dataforts::{
    register_filesystem_blob_adapter, publish_blob, resolve_payload, BlobRef,
};

register_filesystem_blob_adapter("local", "/var/blobs")?;
let blob_ref = publish_blob("local", "local://obj/payload-1", &large_payload).await?;
// blob_ref now rides events as the addressable reference.

let payload = resolve_payload(&blob_ref).await?;
```

```python
from net import register_filesystem_blob_adapter, blob_publish, blob_resolve

register_filesystem_blob_adapter('local', '/var/blobs')
blob_ref = blob_publish('local', 'local://obj/payload-1', large_payload)
payload = blob_resolve(blob_ref)
```

```ts
import { registerFilesystemBlobAdapter, blobPublish, blobResolve } from '@net-mesh/core';

registerFilesystemBlobAdapter('local', '/var/blobs');
const blobRef = await blobPublish('local', 'local://obj/payload-1', largePayload);
const payload = await blobResolve(blobRef);
```

### Custom adapters

Each binding lets you write adapters in the host language:

- **Python** — `register_blob_adapter(id, instance)` where `instance` implements `fetch` / `store` (sync or `async def`). Async adapters run on a binding-owned event loop on a dedicated thread (D-4 fix — no fresh `asyncio.run` per call). An `aiobotocore` / `httpx.AsyncClient` / SQLAlchemy async engine inside the adapter is safe.
- **Node** — `registerBlobAdapter(id, instance)` (sync TSFN bridge) or `registerAsyncBlobAdapter(id, instance)` (Promise-returning TSFN bridge).
- **C / cgo** — `NetBlobAdapterVtable` with per-field null-check at registration (D-22); partial vtables return `NET_ERR_BLOB_VTABLE_INVALID`.

---

## Phase 5 — Read-your-writes

Every successful `Tasks::create` / `Memories::insert` / etc. returns a `WriteToken { origin_hash, seq }`. Pass it to `wait_for_token(token, deadline)` and the call blocks until the local fold has actually applied that sequence number — not just folded it. A producer reads its own write through the cache deterministically; no busy-poll, no time-window heuristic.

This piece composes with `cortex.md` — the WriteToken is what flows out of every CortEX write; the wait_for_token call is what reads block on.

### `WriteToken` shape

```rust
pub struct WriteToken {
    pub(crate) version: u8,
    pub(crate) origin_hash: u64,
    pub(crate) seq: u64,
}
```

Fields are `pub(crate)`; the public constructor is `#[doc(hidden)]`. `FromStr` is gated behind `#[cfg(test)]` / `wire-debug`. **Tokens are unforgeable only against the adapter that issued them** — origin-bound. A token claiming `origin_hash = X` passed to an adapter whose `origin_hash = Y` rejects with `WaitForTokenError::WrongOrigin` (D-19 threat model).

### Wire-up

```rust
let tasks = Tasks::open(redex, channel, origin_hash, cfg)?;
let result = tasks.create(1, "first", net::now_ns())?;
tasks.wait_for_token(result.token, std::time::Duration::from_millis(250)).await?;
// State now reflects the create — read tasks.state() safely.
```

```python
result = tasks.create(1, 'first', now_ns())
tasks.wait_for_token(result.token, deadline_ms=250)
# deadline_ms=0 is a non-blocking poll
```

```ts
const result = tasks.create(1n, 'first', BigInt(now()));
await tasks.waitForToken(result.token, 250);
// deadlineMs === 0 is a non-blocking poll
```

```go
result, _ := tasks.Create(1, "first", uint64(time.Now().UnixNano()))
if err := tasks.WaitForToken(result.Token, 250*time.Millisecond); err != nil { /* … */ }
// PollForToken — non-blocking
// WaitForTokenContext — Go context, but cancellation isn't propagated into the FFI wait (N-11)
```

### Operational notes

- **`applied_through_seq()` vs. `folded_through_seq()`.** The pre-v0.15 wait delegated to `wait_for_seq`, which returned when the *folded* watermark passed `seq` — including events that `FoldErrorPolicy::Skip` silently skipped via `RedexError::is_recoverable_decode`. A producer whose write hit a skip got `Ok(())` and then read state that didn't reflect its write. v0.15 waits on **applied** (events that actually ran through the fold) (D-17 fix).
- **`FoldStopped` is a real error.** `wait_for_seq` used to return `Ok` when `running == false` (fold task crashed under `FoldErrorPolicy::Stop`); every pending RYW wait resolved with silent `Ok(())`. v0.15 surfaces `WaitForTokenError::FoldStopped { applied_through_seq }` (D-18 fix).
- **`deadline_ms == 0` is a non-blocking poll** across every binding. Synchronous applied-vs-token check; no wait scheduled. Pre-fix the FFI rewrote `0` to `1 ms` (D-23, N-4 fixes).
- **Process-wide in-flight cap.** `set_global_ryw_inflight_cap(usize)` sets a process-wide bound on outstanding RYW waiters; every `wait_for_token` does a two-tier acquire (process-wide then per-adapter). The default per-adapter cap is 1024 (renamed `ryw_inflight_cap` with a non-FIFO doc note in D-37) (D-38).
- **Go context cancellation isn't propagated into the FFI wait.** `WaitForTokenContext(ctx, token)` accepts a Go `context.Context` but cancellation doesn't preempt the blocking FFI call — use it for ergonomics, not sub-deadline cancellation (N-11 doc note).

---

## Common gotchas

- **`dataforts` feature must be on.** Builds without it surface typed `RedexError` stubs from every `enable_*` entry point: `"requires the 'dataforts' feature; rebuild with --features dataforts"`. Pre-built release artifacts ship with the feature enabled.
- **Greedy admission rejection has six reasons** (`AdmitRejectReason::{Scope, Proximity, Intent, Colocation, Capacity, Bandwidth}`). Each has its own Prometheus counter — disambiguate "why isn't this chain being cached?" by checking which counter bumped.
- **Gravity without greedy is allowed.** A node with `enable_gravity_for_greedy` but no `enable_greedy_dataforts` is the "drift-only" quadrant — already-placed replicas emit heat, but the node doesn't speculatively cache.
- **`Redex::greedy_cache_for(channel) -> Option<RedexFile>`** returns the cache file if greedy admitted that chain; the caller falls back to a network fetch / substrate read path on `None`. The substrate doesn't auto-route reads through the cache — it's an explicit lookup.
- **Blob refs aren't transactionally tied to the bus.** A `BlobRef` riding on a published event references bytes that the adapter must have stored *before* the event was published; if the consumer reads the event before the adapter persists the bytes, `blob_resolve` fails until the persist completes.
- **`WriteToken` must come from the same adapter you wait on.** Cross-adapter tokens fail with `WrongOrigin`. The token isn't a generic "future state" handle — it's bound to one adapter's fold.

---

## When you need more

- **Full plan + activation gates per phase**: `net/crates/net/docs/misc/DATAFORTS_PLAN.md`.
- **Wishlist audit** (what's a Dataforts phase vs. what already ships via core primitives): `net/crates/net/docs/misc/DATAFORTS_FEATURES.md`.
- **v0.15 release notes** with all the D-1..D-54 + N-1..N-11 fix references: `net/crates/net/docs/releases/RELEASE_v0.15_REBEL_YELL.md`.
- **Cargo feature interaction**: `net` crate's `dataforts` feature pulls `cortex + blake3 + xxhash-rust`. Builds without it get the substrate path unchanged (RedEX, CortEX, NetDB, replication all work as in v0.14).
