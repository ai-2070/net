# Net v0.15 — "Rebel Yell"

*Named after Billy Idol's 1983 album / title track — a release that asks "more, more, more" of the substrate The Warriors laid down. v0.14 made replication the load-bearing layer underneath the channel surface. v0.15 stacks the four-phase Dataforts compositional layer on top: greedy-LRU caching pulls in-scope chains, data gravity drifts hot ones toward their readers, `BlobRef` carries content-addressed pointers without owning the bytes, and read-your-writes gives producers a session-bounded "did my write land yet?" handle. No new wire protocol — every phase composes against the existing capability index, proximity graph, and `causal:` tag layer that landed in The Warriors.*

v0.15 lands **the full Rebel Yell roadmap from [`DATAFORTS_PLAN.md`](../misc/DATAFORTS_PLAN.md)** — Phases 1, 3, 4, and 5 of the seven-phase plan ship in this release, completing Dataforts as a compositional data plane on top of the v0.14 substrate. The full surface ships across Rust core, Python, Node, Go, and C FFI, with end-to-end mesh integration; greedy and gravity are runtime-toggleable policies (operators flip them on / off live against a running mesh, no rebuild required); the single `dataforts` Cargo feature gates whether the surface compiles at all.

The hardening posture from the Black Diamond line continues. Two coordinated code-review passes landed before the v0.15 branch cut: the primary `dataforts-feature` review ([`docs/misc/CODE_REVIEW_2026_05_11_DATAFORTS.md`](../misc/CODE_REVIEW_2026_05_11_DATAFORTS.md)) closed 54 numbered items D-1..D-54, an independent second pass surfaced 11 N-series items, all but three (deferred with rationale) closed. Five post-merge follow-up commits on the `channel-hash-32` branch hardened the RPC-inbound dispatcher hot path and tightened collision-lookup contracts.

Alongside Dataforts, v0.15 carries one cross-cutting breaking change: **the canonical channel hash widens from `u16` to `u32` substrate-wide.** The wire `NetHeader::channel_hash` stays `u16` (the 64-byte cache-line-aligned header is full), mirroring the `origin_hash u64-canonical / u32-wire` precedent. ACL, storage, config, and RYW decisions key on the canonical 32-bit hash; the wire `u16` is a fast-path filter hint only. The `PermissionToken` wire form grows from 159 → 161 bytes.

---

## Greedy-LRU dataforts (Phase 1)

Per-node speculative caching of in-scope chains observed via the tail-subscription path. The mesh fans every event through a `GreedyObserver` hook; the runtime decides whether to admit each event into a per-channel cache file. Cold channels evict under cluster-cap pressure and withdraw their `causal:<hex>` advertisement so peers re-route to a healthy holder. Wires via `Redex::enable_greedy_dataforts(mesh, config, local_caps, intent_registry)`.

### `GreedyConfig`

```rust
pub struct GreedyConfig {
    pub scopes: Vec<ScopeLabel>,           // scope-axis admission
    pub proximity_max_rtt: Option<Duration>, // proximity-axis admission
    pub per_channel_cap_bytes: u64,        // storage-axis admission, per chain
    pub total_cap_bytes: u64,              // cluster-cap eviction trigger
    pub bandwidth_budget_fraction: f32,    // share of measured NIC peak
    pub nic_peak_bytes_per_s: Option<u64>, // operator override of probe
    pub intent_match: IntentMatchPolicy,   // capability-preference axis
    pub colocation_policy: ColocationPolicy, // colocation axis
    pub observer_inflight_cap: usize,      // tokio spawn fan-out bound
}
```

The five admission axes (scope + proximity + capability-preference + colocation + storage-cap) gate every inbound event before the bandwidth-budget gate; rejected events bump the per-reason counter rather than entering the cache file. A bandwidth-budget rejection now increments a distinct `dataforts_greedy_admit_throttled_bandwidth_total` counter (was conflated with capacity-rejects pre-fix) so operators can disambiguate "NIC saturated" from "cache full." `nic_peak_bytes_per_s` overrides the hardcoded 1 Gbps default for fleets with faster NICs.

### Admission + eviction

Inbound events flow through `GreedyRuntime::dispatch_event(channel_name, channel_hash, origin_hash, chain_caps, payload)`. Admission is a pure function — `should_admit(inputs) -> AdmissionVerdict` — that returns one of `Admit` / `RejectedByAdmission(AdmitRejectReason)`. The bandwidth-budget gate runs only after admission passes; admitted events that fail the budget gate are throttled, not silently dropped (D-2). Eviction under the cluster cap returns an `EvictionSweep { evicted: Vec<EvictedEntry> }` value; the runtime calls `sink.withdraw_chain(origin_hash)` for each evicted entry inline so peers see the capability tag drop in the same tick (D-1).

The cache-side `RedexFile` keys on a synthesized `ChannelName` (`dataforts/greedy/<hex16>`) derived from the wire `u16` channel hash; the canonical 32-bit hash decision happens at the ACL / config / RYW layer, not at the data-plane cache (the wire hash is what inbound packets carry). Two channels colliding on the wire `u16` share a cache file — a small mix-up at the data-plane layer; ACL and storage decisions stay collision-safe via the canonical hash.

### TOCTOU + lock-coalescing fixes

`is_new_channel = !cache.contains(channel)` followed by `cache.upsert(...)` previously took two independent lock acquisitions; concurrent dispatch_event calls on the same channel both observed `is_new_channel = true`, both ran `sink.announce_chain`, and the second `upsert` orphaned the first `RedexFile`. v0.15 folds the lazy-open into a single `cache.get_or_insert_with` scope holding the lock for contains / open / upsert; announce fires after lock release. The steady-state path takes one lock; the new-channel path takes two with TOCTOU re-check (D-6, D-28).

`upsert` on an already-registered channel previously refreshed the file pointer without subtracting the prior entry's bytes from `total_bytes`. Reopens via `dispatch_event`'s `is_new_channel` path accumulated `total_bytes` that no eviction could ever drain, eventually starving the cluster-cap budget. The update branch now subtracts the prior `bytes` before replacing the entry's file (D-3).

`entry.bytes` saturates on overflow but didn't reflect retention trim from `RedexFile`. v0.15 ships `RedexFile::retained_bytes` + `GreedyRuntime::resync_cache_bytes` for periodic operator-driven re-anchoring; wiring is opt-in via the operator's tick loop (D-26).

### `colocation_target_held` resolved from cache

`ColocationPolicy::SoftPreference` / `StrictRequired` evaluates whether the local cache already holds chains colocated with the inbound event. The pre-fix `colocation_target_held = None` hardcode caused `StrictRequired` to reject events whose colocation target was actually present locally. The runtime now resolves the colocation target by name against the cache map (D-8).

### Spawn fan-out bound

`observe_event` is the mesh hot-path entry; without a bound, a flooding peer could create one outstanding tokio task per event before the per-event admission lock serialized them, piling up per-task `Bytes` + `Arc<CapabilitySet>` clones. v0.15 ships `observer_inflight: Arc<tokio::sync::Semaphore>` sized via `GreedyConfig::observer_inflight_cap` (default 4096); on saturation, events drop and bump `dataforts_greedy_observer_dropped_overloaded` rather than blocking the mesh dispatch task (D-7).

### Cross-binding API surface

Every binding exposes the same `enable_greedy_dataforts` / `disable_greedy_dataforts` pair plus `greedy_cached_channel_count()` and `greedy_prometheus_text()` for operator scrape. The Go binding carries the runtime-stub fallback (`NET_ERR_FEATURE_NOT_BUILT`) so a cdylib built without the `dataforts` feature still links cleanly into cgo programs (D-20).

---

## Data gravity (Phase 4)

Per-chain read-rate counters with exponential decay. Threshold-crossing emissions stamp `heat:<hex>=<rate>` onto the chain's existing capability announcement; greedy admission weights cache pulls by `heat × scope-match × proximity-rank`. Cold chains evict first under cluster-cap pressure; hot chains migrate toward the readers that drive the heat. No separate migration engine — gravity emerges from greedy + heat counters + capability-preference automatically.

Wires via `Redex::enable_gravity_for_greedy(mesh, DataGravityPolicy)` against an already-running greedy runtime.

### `DataGravityPolicy`

```rust
pub struct DataGravityPolicy {
    pub enabled: bool,
    pub emit_threshold_ratio: f64,         // 1.5 = re-emit when rate is 1.5× last-announced
    pub decay_half_life_secs: u64,         // 300 = 5-minute half-life
    pub tick_interval_ms: u64,             // 5000 = 5-second tick cadence
    pub normalization_reference_rate: f64, // 1000 events/s → 1.0 on the wire
}
```

### Heat counter + emission decision

`HeatCounter::observe(now, weight)` bumps the counter; `HeatCounter::current_rate(now)` returns the decayed rate. `should_emit_heat(prev, current, ratio)` is the pure-logic emission decision: emit when no prior emission, or when the current rate exceeds `prev × ratio` or falls below `prev / ratio`. Edge cases:

- **Near-zero `prev` no longer trips `inf`.** Pre-fix, a `prev` of `1e-300` with finite `current` made `current / prev` evaluate to `+inf`, which trivially satisfied any ratio check. v0.15 treats `prev` below `f64::EPSILON` (and subnormals via `is_normal()`) as "no prior emission" — the bootstrap arm runs cleanly (D-29, N-9).
- **`NaN` rates short-circuit to no-emit.** A NaN slipping into the counter (e.g. via a corrupted `to_le_bytes` round-trip on the wire) used to propagate through the ratio arithmetic. The pure function now returns `EmissionDecision::Skip` on any non-finite input.

### Wire normalization — log-scale

Pre-fix, the wire `heat:<hex>=<rate>` tag normalized `(rate / (rate + 1)).min(1.0)`, which compressed asymptotically — `rate=10` → 0.91, `rate=100` → 0.99. With `{:.2}` wire encoding, every "warm" chain looked like "blazing." v0.15 uses `ln_1p(rate) / ln_1p(reference)` with a configurable `normalization_reference_rate`; the reference defaults to 1000 events/s mapping to 1.0 on the wire. Wire format is unchanged — just the value placed on it (D-30, D-46).

### `HeatRegistry` cap + LRU

`HeatRegistry` previously grew unbounded — one entry per `(channel, origin)` pair the local node ever observed. A misbehaving peer flooding diverse origin hashes could exhaust memory before any greedy-eviction signal fired. v0.15 caps the registry at `DEFAULT_HEAT_REGISTRY_CAP = 8 * 1024` entries with LRU eviction by `last_update`; the tick loop also prunes entries with `rate == 0.0 && last_emitted == Some(0.0)` so cold chains drain on their own (D-10, N-2).

### Inbound `heat:` tag auth

Capability announcements carrying `heat:<hex>` tags previously had no provenance check at the receive side — any peer could emit a heat tag claiming any chain. v0.15 gates inbound heat tags on the publisher's existing `causal:<hex>` claim: a node advertising `heat:X` without simultaneously advertising `causal:X` has its heat tag dropped at the receive boundary (D-11). Per-peer rate-limiting of `heat:` emissions is acknowledged in D-11 + N-8 as deferred — operators see today's posture in [`CODE_REVIEW_2026_05_11_DATAFORTS.md`](../misc/CODE_REVIEW_2026_05_11_DATAFORTS.md).

### `origin_hash == 0` no longer collapses heat

Default-constructed publishers carried `origin_hash = 0`, which collapsed all unattributed chains into a single registry bucket and stamped a meaningless `heat:0000…0000` tag onto the wire. v0.15 stamps `origin_hash` from identity in `publish_to_peer` (the natural fix) and skips heat bumps when `origin_hash == 0` is observed at the gravity-runtime entry as defense-in-depth (D-9).

### `announce_heat_batch` — coalesced rebroadcast

`gravity_tick` previously walked all heat emissions and called `announce_heat` per chain. Each `announce_heat` rewrote the full `CapabilitySet::tags` vector and called `announce_capabilities` — at 100 K chains, O(n² × n_tags) per tick with each emit duplicating all chains' tags on the wire. v0.15 ships `HeatSink::announce_heat_batch`; the tick gathers all emissions, retains all stale heat tags + pushes all new heat tags in one pass, and emits a single `announce_capabilities` (D-25).

---

## BlobRef + BlobAdapter (Phase 3)

Content-addressed reference whose bytes live in the caller's existing storage (S3, Ceph, IPFS, local FS, …). The substrate carries the reference, never owns the bytes. Adapters implement `fetch` / `store` (or the streaming variants for multi-GB payloads); the FileSystemAdapter ships in-tree as the reference adapter.

### Wire format

```text
[0xB0, 0xB1, 0xB2, 0xB3]  // 4-byte magic (was single-byte 0xB0 pre-fix)
version: u8               // currently 1
hash:    [u8; 32]         // BLAKE3
size:    u64              // bytes; bounded by BlobRef::MAX_SIZE = 16 GiB
uri:     [u8]             // length-prefixed; the adapter URI scheme prefix
```

Pre-fix the discriminator was a single byte `0xB0` (D-14). A plain binary payload starting with `0xB0` would misclassify as a blob ref and route through `BlobAdapter::fetch` instead of being delivered directly. The 4-byte magic gives a collision probability of ~1 in 4 billion against arbitrary binary payloads. Old (pre-v0.15) blob refs are rejected on decode; v0.15 nodes can't exchange blob refs with pre-v0.15 nodes (Dataforts is new in v0.15, so this only matters for pre-release pilots).

`BlobRef::MAX_SIZE = 16 GiB` defaults bound the `size` field; `BlobRef::decode` and `publish_blob` reject anything larger. The previous `u64::MAX` accept-anything path could OOM on `vec![0u8; len as usize]` on 64-bit and silently truncate on 32-bit (D-15). `RedexFileConfig::blob_max_size` lifts the cap when an operator needs it.

### Adapter dispatch — URI-scheme keyed

`BlobAdapter::accepted_schemes() -> &[&str]` declares the URI schemes the adapter handles (`["s3", "s3+https"]`, `["file"]`, etc.); the registry dispatches by URI scheme, not by the channel config's `blob_adapter_id`. Pre-fix, an attacker who could write to a channel could choose its `blob_adapter_id` and route a `BlobRef` URI through any registered adapter — authority confusion (D-13). The scheme-keyed dispatch closes the gap; the channel-config-selected path is gone.

### Hash verification on store

`FileSystemAdapter::store(blob_ref, &bytes)` now BLAKE3-hashes the supplied bytes and rejects on mismatch with `blob_ref.hash`. Pre-fix the adapter wrote whatever bytes the caller passed; a content-address-violating store would silently corrupt the addressable layer (D-12). The rename-fallback path (idempotent re-store on existing content) also hash-verifies the on-disk bytes — the v0.13/v0.14-era TOCTOU on idempotent re-store via the windowed rename is closed (D-32, N-6).

`fsync` of the temp file before rename + `fsync` of the parent dir after rename land in the FileSystemAdapter store path; power loss between `rename` and OS flush previously left zero-length files in the addressable space (D-33).

### Streaming hooks

`fetch_stream(&self, blob: &BlobRef) -> Pin<Box<dyn Stream<Item = Result<Bytes>> + Send>>` and `store_stream(&self, blob: &BlobRef, src: Pin<Box<dyn Stream<...>>>)` ship as required methods on `BlobAdapter` with default implementations that route through the existing `fetch` / `store` (so existing impls keep working); adapters wanting real streaming override the defaults. The FileSystemAdapter chunks at 256 KiB (D-16).

### Per-channel adapter override (multi-tenant)

`BlobAdapterRegistry` previously lived as a single process-wide singleton. v0.15 adds `RedexFileConfig::blob_adapter_registry: Option<Arc<BlobAdapterRegistry>>` for per-channel override; the default-tenant path uses the global singleton unchanged (D-34).

### Bounded concurrency on the FS adapter

`spawn_blocking` calls on the FileSystemAdapter are bounded via `tokio::sync::Semaphore`. Pre-fix, a fanout of concurrent stores could exhaust the tokio blocking pool and deadlock unrelated tasks (D-35).

### Conformance suite

The blob adapter conformance suite extends to cover idempotency (re-store same hash), hash-mismatch rejection, range-past-end behavior, cross-blob isolation (writes to blob A can't leak into blob B's namespace), and random-ghost reads (resolve a never-published BlobRef). Adapter authors pin against the same suite the in-tree FileSystemAdapter does (D-36).

### Cross-binding adapter authoring

Adapters can be written in the host language across every binding:

- **Python** — `PyBlobAdapter` with sync + `async def` method support. Async adapters run on a binding-owned event loop on a dedicated thread (one loop per process); calling thread sharing is via `asyncio.run_coroutine_threadsafe`. An `aiobotocore` / `httpx.AsyncClient` / SQLAlchemy async engine inside the adapter is safe — the binding never spins up a fresh `asyncio.run` per call (D-4).
- **Node** — `NodeBlobAdapter` (sync TSFN bridge) + `NodeAsyncBlobAdapter` (Promise-returning TSFN bridge).
- **C / cgo** — `NetBlobAdapterVtable` with per-field null-check at registration; partial vtables return `NET_ERR_BLOB_VTABLE_INVALID` rather than crashing on first dispatch (D-22).

`BlobError::NotFound(uri)` sanitizes the URI before including it in the error string — control chars escape as `\xNN`, length caps at 256 bytes — so a binding logging the error can't be log-injected by an attacker who controls the URI (D-31).

---

## Read-your-writes (Phase 5)

Every successful `Tasks::create` / `Memories::insert` / etc. returns a `WriteToken { origin_hash, seq }`. Pass it to `wait_for_token(token, deadline)` and the call blocks until the local fold has actually applied that sequence number — not just folded it. A producer reads its own write through the cache deterministically; no busy-poll, no time-window heuristic.

### `WriteToken`

```rust
pub struct WriteToken {
    pub(crate) version: u8,
    pub(crate) origin_hash: u64,
    pub(crate) seq: u64,
}
```

Fields are `pub(crate)`; the public constructor is `#[doc(hidden)]`. `FromStr` is gated behind `#[cfg(test)]` or the `wire-debug` feature. Tokens are unforgeable only against the adapter that issued them (via origin binding); the threat model is documented inline (D-19).

### `wait_for_token` — applied vs. folded

Pre-fix, `wait_for_token` delegated to `wait_for_seq`, which returned when the folded watermark passed `seq` — including events that `FoldErrorPolicy` silently skipped via `RedexError::is_recoverable_decode`. A producer whose write hit a skip got `Ok(())` and then read state that didn't reflect its write.

v0.15 adds `applied_through_seq()` (events that actually ran through the fold) alongside the existing `folded_through_seq()` (events the fold saw). `wait_for_token` waits on **applied**, not folded; skipped events are no longer auto-acknowledged (D-17).

### `FoldStopped` error variant

`wait_for_seq` previously returned `Ok` when `running == false` (the fold task crashed under `FoldErrorPolicy::Stop`). Every pending RYW wait resolved with a silent `Ok(())` even though `seq` was never folded. v0.15 adds `WaitForTokenError::FoldStopped { applied_through_seq }`; the wait path checks `applied_through_seq >= seq` when it wakes due to `running == false` and surfaces the typed error when the fold actually stalled (D-18).

### Non-blocking poll — `deadline_ms == 0`

`wait_for_token(token, 0)` now does a synchronous applied-vs-token check and returns `Ok(())` / `Err(Timeout)` / `Err(FoldStopped)` without scheduling a wait. Pre-fix the FFI rewrote `0` to `1 ms`, costing a real wait round-trip for a "is fold caught up?" probe (D-23). The synchronous-poll behavior is consistent across the FFI / Node / Go / Python surfaces; the Python surface promoted `poll_for_token` to the public API alongside `wait_for_token` so non-async Python callers can probe without spawning a task (N-4).

### Process-wide in-flight cap

The 1024-deep wait-queue cap was per-adapter pre-fix; a process with 100 channels could stack 100 K outstanding RYW waiters. v0.15 ships `set_global_ryw_inflight_cap(usize)` for a process-wide bound; every `wait_for_token` call does a two-tier acquire (process-wide first, then per-adapter). The semaphore is renamed `ryw_inflight_cap` with a non-FIFO documentation note (the current implementation is `Semaphore::try_acquire`; true FIFO is deferred) (D-37, D-38).

### Cross-binding API surface

| Binding | Surface |
|---------|---------|
| **Rust** | `tasks.wait_for_token(token, Duration)` / `memories.wait_for_token(token, Duration)`; `tasks.poll_for_token(token)` synchronous variant |
| **Python** | `tasks.wait_for_token(token, deadline_ms=…)`; `deadline_ms=0` is a non-blocking poll (N-4) |
| **Node** | `tasks.waitForToken(token, deadlineMs)`; `deadlineMs === 0` is a non-blocking poll |
| **Go** | `tasks.WaitForToken(token, timeout)` + `tasks.PollForToken(token)` + `tasks.WaitForTokenContext(ctx, token)` non-blocking variant; Go context cancellation isn't propagated into the FFI wait — see `WaitForTokenContext` rustdoc for the contract (D-45, N-11) |
| **C** | `net_tasks_wait_for_token` / `net_memories_wait_for_token`; `timeout_ms == 0` is a non-blocking poll. Every FFI entry wraps `block_on` in `std::panic::catch_unwind(AssertUnwindSafe(…))`; panics surface as `NET_ERR_PANIC` rather than unwinding across `extern "C"` (D-21) |

---

## Channel-hash widening — `u16` → `u32` canonical

The wire `NetHeader::channel_hash` (16 bits, 65 536 buckets) routinely collides at mesh scale — birthday-paradox threshold ~300 channels. Pre-fix every substrate decision keyed on the wire `u16`: ACL (AuthGuard), storage (Redex), config (ChannelConfigRegistry), token (PermissionToken), RYW. Two unrelated channels colliding on `u16` shared one ACL decision, one RedexFile, one config row.

v0.15 widens the canonical channel hash to **`u32` substrate-wide** while keeping the wire `NetHeader::channel_hash` at `u16` — the per-packet width is fixed by the 64-byte cache-line-aligned header budget. The wire `u16` is now a fast-path filter hint only; wire-side collisions are benign because every non-fast-path decision (auth / storage / config / RYW) keys on the canonical 32-bit hash via registry-side disambiguation.

Mirrors the `origin_hash u64-canonical / u32-wire` precedent set in v0.13: per-packet width fixed, application layer wider, narrowing helper at the wire boundary.

### Canonical type

```rust
pub type ChannelHash = u32;

impl ChannelName {
    pub fn hash(&self) -> ChannelHash { … }        // canonical u32
    pub fn wire_hash(&self) -> u16 { … }            // wire fast-path hint
}
pub fn channel_hash(name: &str) -> ChannelHash { … }
pub fn wire_channel_hash(name: &str) -> u16 { … }
```

`ChannelHash` joint-collision threshold is ~65 K channels per process (above realistic deployment), so the canonical key is treated as collision-free in fast paths.

### `ChannelConfigRegistry` — dual index

```rust
pub struct ChannelConfigRegistry {
    configs: DashMap<String, ChannelConfig>,
    by_hash: DashMap<ChannelHash, Vec<String>>,    // canonical (u32, rare collisions)
    by_wire_hash: DashMap<u16, Vec<String>>,       // wire (u16, routine collisions)
    prefix_configs: DashMap<String, ChannelConfig>,
}
```

`get(canonical)` returns `None` on the rare canonical collision (forces caller fallback); `get_by_wire_hash(wire)` returns `None` on wire-bucket collision (used by receive-side dispatch, contrast with `ChannelRegistry::get_all_by_wire_hash` below). Removals stay collision-safe — `remove(canonical)` keys on the unique canonical hash; `remove_by_name(name)` is the explicit-name path.

### `ChannelRegistry` — return the full collision set

`ChannelRegistry::get_by_wire_hash` was renamed to `get_all_by_wire_hash` and explicitly returns the full collision-bucket vector. This contrasts with `ChannelConfigRegistry::get_by_wire_hash`, which returns `None` on collision to force a safe default at the policy layer. The naming asymmetry is intentional — operators querying "what channels share this wire bucket" want the full set; the policy layer querying "what's the config for this packet" wants a unique answer or nothing.

### `AuthGuard` — canonical u32 ACL

The bloom-filter key buffer widens from 10 to 12 bytes (`u64 origin_hash + u32 channel_hash`); `check_fast` / `authorize` / `revoke` signatures all take `ChannelHash`. The two-tier authorization shape (fast-path bloom + verified cache + exact-name backstop) is unchanged; the canonical hash makes the fast-path bloom collision-resistant at realistic scale. The exact-name backstop remains the only collision-free path for control-plane / storage authorization decisions where adversarial canonical-hash collisions matter.

### `PermissionToken` — 161-byte wire form

```text
issuer:           32 bytes (EntityId)
subject:          32 bytes (EntityId)
scope:             4 bytes (u32)
channel_hash:      4 bytes (canonical ChannelHash, u32; was u16)
not_before:        8 bytes (u64 unix timestamp)
not_after:         8 bytes (u64 unix timestamp)
delegation_depth:  1 byte  (u8)
nonce:             8 bytes (u64)
--- signed ---
signature:        64 bytes (ed25519)
```

Total: 161 bytes (was 159). Signed payload: 97 bytes (was 95). `PermissionToken::from_bytes` rejects 159-byte input as `TokenError::InvalidFormat`; old tokens must be reissued under the wider form.

### RPC inbound dispatcher — `(canonical, dispatcher)` pairs

`MeshNode::register_rpc_inbound(channel_hash: ChannelHash, dispatcher)` takes the canonical hash. The dispatcher map is indexed by the wire `u16` for O(1) lookup on the inbound packet decode path; each bucket stores a `Vec<(ChannelHash, RpcInboundDispatcher)>` so wire-bucket collisions between independently-registered canonical channels don't share a dispatcher slot. `RpcInboundEvent::channel_hash` is the canonical `u32` — dispatchers receive the disambiguated identity.

### Five post-merge follow-up commits

A focused review pass landed five hardening commits on the `channel-hash-32` branch after the primary widening:

| # | Commit | Concern | Test added |
|---|---|---|---|
| 1 | `fda25a7d` | Race in `unregister_rpc_inbound` clobbering a concurrent sibling register | sibling-survives + race-stress |
| 2 | `af5f6c25` | Stale "16-bit verified cache" comment in `mesh.rs` | n/a (doc) |
| 3 | `1fb62fbc` | Stale 16-bit framing in two regression test docstrings | n/a (doc) |
| 4 | `c141e691` | Per-packet `Vec` allocation in the dispatch fast path | end-to-end wire-bucket collision fan-out |
| 5 | `c5e75ff6` | `get_by_wire_hash` semantics divergence between registries | full-collision-set contract test |

The single-dispatcher hot path (the overwhelming case at typical sizing) avoids the heap allocation entirely; the collision-set vector is built only when a wire bucket has more than one canonical entry. The unregister race is closed by atomic remove-if-present semantics; concurrent register + unregister can no longer leave the map in a torn state.

### Dataforts greedy stays on wire `u16`

The greedy data-plane cache deliberately keys on the wire `u16` (not the canonical `u32`) because the wire hash is what the inbound packet that triggered the observe call carries — there's no canonical lookup at packet decode time. The cache file is named `dataforts/greedy/<hex16>`; two wire-colliding channels share a cache file (a small mix-up at the data-plane layer; ACL and storage decisions stay collision-safe via the canonical hash).

---

## Hardening — `dataforts-feature` two-pass review

Two coordinated review passes landed before the v0.15 branch cut. The primary review on the `dataforts-feature` branch surfaced 54 numbered items (D-1..D-54): 4 blockers, 19 highs, 24 mediums, 7 lows. An independent second pass on 2026-05-12 surfaced 11 N-series items (N-1..N-11): 3 highs, 6 mediums, 2 lows. All but three closed before merge (deferred with rationale in the tracking doc). The closures group by area:

### Greedy correctness (D-1..D-8 + D-25..D-28 + N-5)

- Cluster-cap eviction withdraws chain announcements inline (D-1).
- Bandwidth-budget rejection bumps a distinct counter rather than dropping events silently (D-2).
- `upsert` on update subtracts the old `bytes` before replacing the file pointer (D-3).
- `chain_caps` resolves the chain publisher's caps via the capability index, not the last-hop peer's (D-5).
- TOCTOU on `is_new_channel` collapsed into a single locked get-or-insert (D-6).
- `tokio::spawn` per inbound event bounded by a semaphore (D-7).
- `colocation_target_held` resolved from the cache map, not hardcoded `None` (D-8).
- `gravity_tick` coalesces N×`announce_capabilities` into one via `announce_heat_batch` (D-25).
- Retention-trim drift on `entry.bytes` resyncs via `RedexFile::retained_bytes` (D-26).
- 5 cache-lock acquisitions per dispatch coalesced to 1 in the steady-state path; new-channel path takes 2 with TOCTOU re-check (D-28).
- Eviction explicitly drops `gravity.heat` lock before calling `sink.withdraw_chain` to avoid lock-ordering hazards (N-5).

### Gravity correctness (D-9..D-11 + D-29..D-30 + N-2 + N-9)

- `origin_hash == 0` no longer collapses per-chain heat (D-9, fix at publish-side + defense-in-depth at gravity-runtime entry).
- `HeatRegistry` bounded + LRU-evicted + tick-pruned (D-10, N-2).
- Inbound `heat:` tags gated on the publisher's matching `causal:` claim (D-11; per-peer rate-limit deferred per N-8).
- `should_emit_heat` subnormal-safe via `is_normal()` + EPSILON-floor (D-29, N-9).
- Log-scale wire normalization with configurable reference rate (D-30 / D-46).

### Blob correctness (D-12..D-16 + D-31..D-36 + D-49..D-50 + D-52..D-53 + N-3 + N-6 + N-7)

- `FileSystemAdapter::store` hash-verifies bytes (D-12).
- URI-scheme keyed adapter dispatch closes authority confusion (D-13).
- 4-byte magic for `BlobRef` discriminator closes payload-misclassification (D-14).
- `BlobRef` size bounded; `fetch_range` guards `usize` cast (D-15).
- Streaming hooks on `BlobAdapter` (D-16).
- Log injection via `BlobError::NotFound(uri)` sanitized (D-31).
- Unique-suffix temp filenames (D-32); `fsync` of temp + parent dir (D-33).
- Per-channel `BlobAdapterRegistry` override (D-34).
- Bounded concurrency on `spawn_blocking` (D-35).
- Conformance suite extended with idempotency / hash-mismatch / range-past-end / cross-blob isolation / random-ghost (D-36).
- `BlobError` marked `#[non_exhaustive]` (D-49).
- `RedexFileConfig::blob_adapter_id` unset surfaces the right error variant (D-50).
- `OpaqueCtx(AtomicPtr<c_void>)` collapsed to plain `*mut c_void` (D-52).
- Adapter timeout user-tunable (D-53).
- `path_for` defends against symlinks in the shard root via canonicalize (N-3).
- Windows rename-fallback TOCTOU on idempotent re-store hash-verifies existing content (N-6).
- `catch_unwind` + caller-held locks documented as a hazard in `ffi/mod.rs` + per-binding READMEs (N-7).

### RYW correctness (D-17..D-19 + D-37..D-38 + D-45 + D-51 + N-4 + N-11)

- `wait_for_token` waits on applied seq, not folded (D-17).
- `FoldStopped` error variant when fold task crashes mid-wait (D-18).
- `WriteToken` doc-hidden constructor + threat-model docstring (D-19).
- `ryw_inflight_cap` rename + non-FIFO doc note (D-37).
- Process-wide `set_global_ryw_inflight_cap` with two-tier acquire (D-38).
- Go binding lands `Tasks` + `Memories` adapters with `WaitForToken` + `PollForToken` + `WaitForTokenContext` (D-45).
- `wait_duration_nanos_sum` saturating u128 → u64 cast (D-51).
- Python `wait_for_token(deadline_ms=0)` non-blocking poll consistent with FFI / Node / Go (N-4).
- Go context cancellation doc contract clarified (N-11).

### FFI / cross-binding (D-20..D-23 + D-39..D-44 + D-54 + N-1 + N-10)

- cgo externs link cleanly without `dataforts` feature via `NET_ERR_FEATURE_NOT_BUILT` stubs (D-20).
- Panics across FFI caught + remapped to `NET_ERR_PANIC` (D-21).
- Vtable per-field null-check (D-22).
- `timeout_ms == 0` honored as non-blocking poll (D-23).
- `mesh_arc` drop coverage via RAII guard rather than duplicated drop-on-error (D-39).
- Node `await_tsfn_promise` applies the 30 s timeout once (was 30 s × 2 → 60 s worst case) (D-40).
- Node `DataGravityConfigJs` `*_secs / _ms` widths match the Rust + Python + Go peers (D-41).
- Python `Py<PyAny>` adapters can no longer outlive interpreter finalization (D-42).
- Python adapter `data.to_vec()` copies inside `py.detach` (D-43, N-1).
- Go `omitempty` doc note on greedy / gravity numeric fields (D-44 deferred — substrate rejects `0` for every affected field; `omitempty` is correct).
- Go `runtime.SetFinalizer` runs blocking `Close` on the GC thread — doc note rather than refactor (D-54).
- Python `atexit` drain counts drained vs. missing entries via `NET_PY_TRACE_ATEXIT` env var (N-10).

### Hygiene (D-47..D-48)

- `metrics.rs` channel-cap race doc note (D-47).
- `_force_use_hashmap` dead allow removed (D-48).

The deferred N-8 (per-peer rate-limiting of `heat:` tags) is acknowledged in D-11 and tracked for a separate slice; the auth-via-causal-claim gate forecloses the dominant attack vector today.

---

## Test hygiene

- **Lib suite at 2645+ tests** (was 2640+ at v0.14 release). 60+ net new tests across the four Rebel Yell phases + the channel-hash widening; every numbered review item ships with at least one regression where the shape made one possible. Notable additions: greedy admission + eviction unit coverage, gravity heat-counter decay + emission edge cases, blob conformance suite, RYW applied-vs-folded watermark separation, channel-hash canonical-vs-wire collision tests, RPC dispatcher race-stress + sibling-survives.
- **Cross-binding wire-format fixtures regenerate** against the 161-byte token wire form. The 159-byte token vectors under `tests/cross_lang_capability/` rename and re-encode; binding-side tests that hardcoded the 159-byte length update accordingly.
- **`cargo clippy --all-features --all-targets -D warnings` clean** across substrate + every binding crate.
- **`cargo doc --all-features --no-deps` clean under `RUSTDOCFLAGS="-D warnings"`** — `rustdoc::broken_intra_doc_links` and `rustdoc::private_intra_doc_links` both enforce.
- **Go `go vet ./...` clean** under `CGO_ENABLED=1`; the pre-existing `testOrigin` `uint32` / `uint64` mismatch in `cortex_test.go` is fixed alongside the FFI `net_channel_hash` `u16 → u32` change.

---

## Breaking changes

### Wire format — `PermissionToken` is 161 bytes

`PermissionToken::WIRE_SIZE` grows from 159 → 161 bytes; the signed payload grows from 95 → 97 bytes. `PermissionToken::from_bytes` rejects 159-byte input as `TokenError::InvalidFormat`. Old tokens must be reissued; mixed v0.14 / v0.15 fleets cannot exchange tokens. Recommend lockstep upgrade.

### Wire format — `BlobRef` magic widens to 4 bytes

`BlobRef::MAGIC = [0xB0, 0xB1, 0xB2, 0xB3]`. Pre-v0.15 1-byte-discriminator blob refs (if any pilot deployment serialized them) are rejected on decode. Dataforts is new in v0.15, so this only matters for pre-release pilots.

### API — `ChannelHash = u32` substrate-wide

- **`ChannelName::hash()` returns `u32`** (was `u16`). New `ChannelName::wire_hash() -> u16` exposes the wire fast-path hint.
- **`channel_hash(name: &str) -> u32`** (was `u16`). New `wire_channel_hash(name: &str) -> u16`.
- **`AuthGuard::{check_fast, authorize, revoke, is_authorized}` take `ChannelHash`** (was `u16`).
- **`PermissionToken::channel_hash` is `u32`** (was `u16`); `TokenScope::with_channel`, `try_issue`, `TokenCache::{check, get}` all widen.
- **`MeshNode::register_rpc_inbound` takes `ChannelHash`** (was `u16`); `RpcInboundEvent::channel_hash` is `u32`.
- **`ChannelConfigRegistry::{get, remove, priority}` take `ChannelHash`**; new `get_by_wire_hash(u16)` for receive-side disambiguation.
- **`ChannelRegistry::get_by_wire_hash` renamed to `get_all_by_wire_hash`** and explicitly returns the full collision-bucket vector.

### FFI — `net_channel_hash` takes `uint32_t*`

```c
// v0.14
int net_channel_hash(const char* channel, uint16_t* out_hash);
// v0.15
int net_channel_hash(const char* channel, uint32_t* out_hash);
```

Go / Python / Node bindings widen their `channel_hash` / `channelHash` exports to `uint32` / `int` (u32 range) / `number` (u32 range). `TokenInfo.channel_hash` fields widen to match.

### API — Dataforts surface is new

`Redex::enable_greedy_dataforts(mesh, GreedyConfig, local_caps, IntentRegistry)`, `Redex::disable_greedy_dataforts()`, `Redex::enable_gravity_for_greedy(mesh, DataGravityPolicy)`, `Redex::disable_gravity_for_greedy()`, `BlobAdapterRegistry`, `BlobRef`, `BlobAdapter` trait, `WriteToken`, `tasks.wait_for_token` / `memories.wait_for_token` are all new in v0.15. Behind the `dataforts` Cargo feature; non-dataforts builds see typed `RedexError` stubs ("requires the `dataforts` feature; rebuild with --features dataforts") rather than a silent no-op.

### Behavioral fixes that may surface as test breakage

- **Greedy `dispatch_event` is now lock-coalesced.** Tests that asserted on the pre-fix 5-lock-per-dispatch behavior will see 1 lock in the steady state, 2 in the new-channel path.
- **`HeatRegistry` is capped at 8 K entries.** Tests that fill the registry with > 8 K entries to observe unbounded growth will see LRU eviction.
- **`should_emit_heat` returns `Skip` on near-zero `prev`.** Tests that injected `prev = 1e-300` to observe the pre-fix `inf`-prone branch will see the bootstrap arm instead.
- **`wait_for_token` returns `Err(WaitForTokenError::FoldStopped)`** when the fold task crashed mid-wait. Tests that asserted `Ok(())` against a fold-stopped adapter will see the typed error.
- **`wait_for_token(token, 0)` is a non-blocking poll** across every binding. Tests that injected 0 expecting a real `1 ms` wait will see the synchronous return.
- **`PermissionToken::from_bytes` rejects 159-byte input.** Tests that hardcoded the 159-byte wire form will see `TokenError::InvalidFormat`.

---

## How to upgrade

1. **Bump your `Cargo.toml` / `package.json` / `requirements.txt` / `go.mod` to the v0.15 line.** Recompile / rebuild the binding cdylib (NAPI for Node, maturin for Python, `cargo build -p net-compute-ffi` + `-p net-rpc-ffi` for Go) with the `dataforts` Cargo feature on (pre-built release artifacts ship with the feature enabled).
2. **Channel-hash type migration.** Use `ChannelHash` (`u32`) for ACL / storage / config / RYW decisions; use `ChannelName::wire_hash()` / `wire_channel_hash()` for the 16-bit header value when constructing wire-level packets. The renames are compile errors — `cargo build` (and the binding-side TypeScript / Python static checks) drives the rewrite.
3. **Token reissue.** `PermissionToken` wire form is 161 bytes. Reissue tokens to clients; pre-v0.15 159-byte tokens are rejected on decode. The signed-payload field shifts mean old signatures don't verify against the new layout — there's no in-place upgrade.
4. **Greedy opt-in.** Channels that want greedy caching: call `Redex::enable_greedy_dataforts(mesh, GreedyConfig, local_caps, IntentRegistry)` once after constructing the `Redex` (idempotent). The runtime registers a `GreedyObserver` on the mesh's inbound dispatch; admission decisions run per inbound event without any per-channel opt-in. `Redex::disable_greedy_dataforts()` removes the observer.
5. **Gravity opt-in.** Layer gravity on top of greedy with `Redex::enable_gravity_for_greedy(mesh, DataGravityPolicy)`. The tick loop spawns automatically; tune `decay_half_life_secs`, `tick_interval_ms`, `emit_threshold_ratio`, `normalization_reference_rate` to match the deployment's read-rate skew.
6. **Blob adapter registration.** For channels that publish payloads above the inline threshold: register an adapter (`register_filesystem_blob_adapter(id, root)` for the in-tree FS adapter; `register_blob_adapter(id, instance)` for a host-language adapter), then `blob_publish(adapter_id, uri, bytes)` / `blob_resolve(blob_ref)` against the registered URI scheme.
7. **RYW opt-in.** Capture the `WriteToken` returned from every `tasks.create` / `memories.insert`; pass it to `tasks.wait_for_token(token, deadline)` before reading state that needs to reflect the write. `deadline_ms = 0` is a non-blocking poll.
8. **Operator dashboards.** `Redex::greedy_prometheus_text()` emits per-channel greedy metrics in Prometheus text format. Heat emissions ride the existing capability-announcement metrics — `dataforts_gravity_emit_total`, `dataforts_gravity_heat_registry_size`, etc.
9. **Single-binding deployments without dataforts.** Builds without the `dataforts` Cargo feature surface typed `RedexError` stubs from every `enable_*` entry point. The substrate substrate path is unchanged — RedEX, CortEX, NetDB, replication all work as in v0.14.
10. **Cross-binding wire fixtures regenerated.** If you have CI that asserts golden-vector parity against `tests/cross_lang_capability/`, the 161-byte token form means token-bearing fixtures change. The blob-ref fixtures land for the first time in v0.15.
11. **FFI consumers (C / cgo).** `net_channel_hash` takes `uint32_t*` (was `uint16_t*`). The four new Dataforts entry points (`net_redex_enable_greedy_dataforts`, `net_redex_disable_greedy_dataforts`, `net_redex_enable_gravity_for_greedy`, `net_redex_disable_gravity_for_greedy`) follow the existing `net_redex_*` shape. Without the `dataforts` feature, the symbols return `NET_ERR_FEATURE_NOT_BUILT` rather than failing to link.
12. **Mixed v0.14 / v0.15 fleets.** Replication traffic continues to work cross-version (the `SUBPROTOCOL_REDEX` wire format is unchanged). Tokens do not (161 vs 159 bytes). Recommend lockstep upgrade for any deployment using `PermissionToken`-bearing channels.

---

Released 2026-05-12.

## License

See [LICENSE](../../LICENSE).
