# Performance Analysis: Dataforts (Blob, Greedy, Gravity)

Supplemental to the unified report. Focuses on the dataforts layer — the compositional features built on top of the substrate primitives. Items continue from #170.

The frequency profile here is unusual: most code is per-blob-operation (cold relative to per-event paths), but a few spots are per-event (greedy cache `dispatch_event`) or per-chunk during bulk operations. The findings cluster around hex encoding, sequential fetch I/O that should be parallel, and per-event mutex chains.

---

## ✅ Fixed

| # | Item | Notes |
|---|------|-------|
| 171 | `hex32` and `chunk_channel` `write!`-per-byte → lookup-table encoder | New `HEX_LOWER` table + `hex32_into(&[u8; 32], &mut [u8; 64])` zero-allocation form; `hex32` allocates one 64-byte `String` via `from_utf8` (validator is SIMD-fast on short ASCII). `chunk_channel` builds the bytes directly with `extend_from_slice + hex32_into` instead of looping through the `core::fmt::Arguments` formatter. The duplicate `hex32` in `error.rs` now delegates to the shared one. Pre-fix did 32 dispatches through `write!("{:02x}", b)` per call — roughly 10× slower for the same output. Pinned by `hex32_matches_write_macro_output_byte_for_byte` (byte-for-byte parity with legacy on four corner-case inputs) and `hex32_into_matches_hex32_output`. |

---

## 🔴 High-impact

### 171. `hex32` and `chunk_channel` use `write!` per byte for hex encoding — called throughout the blob layer

**Location:** `blob/mod.rs:42-49` and `blob/mesh.rs:2321-2329`:
```rust
pub(crate) fn hex32(hash: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in hash {
        use std::fmt::Write;
        let _ = write!(s, "{:02x}", b);
    }
    s
}

fn chunk_channel(hash: &[u8; 32]) -> ChannelName {
    let mut name = String::with_capacity(CHUNK_CHANNEL_PREFIX.len() + 64);
    name.push_str(CHUNK_CHANNEL_PREFIX);
    for b in hash {
        use std::fmt::Write;
        let _ = write!(name, "{:02x}", b);
    }
    ChannelName::new(&name).expect(...)
}
```

Both invoke `write!` 32 times, dispatching through `core::fmt::Arguments` formatting machinery per byte. For each `fetch_chunk` call, `chunk_channel` is called once. For each error path, `hex32` may be called multiple times for the URI string.

The doc explicitly says hex32 is "used throughout the blob layer for channel names, `mesh://<hex>` URIs, log lines, and operator output." Every blob operation that touches a chunk pays one or more of these.

For a manifest fetch of N chunks: N×`chunk_channel` calls = N×(64 write! invocations + String alloc + ChannelName validation).

**Fix:** Lookup-table hex encoding:
```rust
const HEX: &[u8; 16] = b"0123456789abcdef";
fn hex32_into(hash: &[u8; 32], buf: &mut [u8; 64]) {
    for (i, b) in hash.iter().enumerate() {
        buf[i*2]   = HEX[(b >> 4) as usize];
        buf[i*2+1] = HEX[(b & 0xf) as usize];
    }
}
```

Or use the `hex` crate's `encode_to_slice`. Roughly 10× faster than the `write!` loop and zero allocator pressure if the caller owns the buffer.

For high-rate blob fetching (e.g. resolve_payload over a manifest blob during event processing), this is the dominant CPU cost on the cold-data path.

### 172. `MeshBlobAdapter::fetch` fetches manifest chunks SEQUENTIALLY

**Location:** `blob/mesh.rs:3034-3055`:
```rust
let mut out: Vec<u8> = Vec::new();
let mut err: Option<BlobError> = None;
for chunk in chunks {
    match self.fetch_chunk(&chunk.hash).await {
        Ok(chunk_bytes) => out.extend_from_slice(&chunk_bytes),
        Err(e) => { err = Some(e); break; }
    }
}
```

For a manifest blob with N chunks: N round-trips through the redex layer, one at a time. If the chunks need to be fetched from peers (replicated blob, partial local availability), each fetch is a network round-trip serialized.

For a 64MB blob with 64KB chunks = 1024 chunks. Sequential at 1ms per fetch = 1 second total. Parallel with concurrency 16 = ~64ms. **15× speedup** on bulk fetch latency for replicated blobs.

**Fix:** Use `futures::stream::iter(chunks).buffered(N)` or `FuturesUnordered` with bounded concurrency. Bound by `BlobBandwidth` if it exists, otherwise a fixed concurrency like 16.

```rust
use futures::{stream, StreamExt};
let bytes_vec: Result<Vec<Vec<u8>>, _> = stream::iter(chunks.iter())
    .map(|chunk| self.fetch_chunk(&chunk.hash))
    .buffered(16)
    .collect::<Vec<_>>()
    .await
    .into_iter()
    .collect();
```

Need to preserve order — `buffered` does that; `buffer_unordered` does not. For blob assembly, order matters.

Combined with #171 (faster channel name construction), the bulk-fetch path becomes much faster for large blobs.

### 173. `MeshBlobAdapter::store` Manifest path is also sequential and double-hashes

**Location:** `blob/mesh.rs:2938-2976`:
```rust
let recomputed = chunk_payload(bytes)?;   // hashes every chunk
// ...
for (i, (recomputed_chunk, chunk_bytes)) in recomputed_chunks.iter().enumerate() {
    // ... verification ...
    self.store_chunk(&recomputed_chunk.hash, chunk_bytes).await?;
}
```

Two issues:

**Issue A:** `chunk_payload(bytes)` BLAKE3-hashes every chunk sequentially before any storing happens. Then `store_chunk` runs sequentially per chunk. For an N-chunk store, that's N hashes + N store calls, all serial.

**Issue B:** The hash work duplicates the caller's hashing — the caller built the Manifest BlobRef with hashes, then we recompute the same hashes to verify the caller didn't lie. Necessary for safety, but means store cost is 2x the raw write cost.

**Fixes:**
- **Parallelize stores:** Same `buffered(N)` pattern as #172.
- **Parallelize hashing:** BLAKE3's `Hasher::update_rayon` parallelizes hashing of large inputs. For chunk-level work, dispatch chunks across rayon's pool.
- **Skip recompute if caller is trusted:** Add a `store_verified` variant for callers (e.g., internal substrate paths) that have already verified hashes. Public API stays safe; internal paths get the speedup.

### 174. `BlobRef::encoded_len` does a FULL re-encode for Manifest/Tree variants

**Location:** `blob/blob_ref.rs:547-552`:
```rust
pub fn encoded_len(&self) -> usize {
    match self {
        Self::Small { uri, .. } => BLOB_REF_SMALL_HEADER_LEN + uri.len(),
        Self::Manifest { .. } | Self::Tree { .. } => self.encode().len(),
    }
}
```

For a Manifest with 1000 chunks, `encode()` allocates a Vec, postcard-serializes the entire manifest into it, then we read `.len()` and throw the Vec away. **The function name implies "cheap measurement"; the implementation is "encode and discard."**

The doc warns about this: "Callers in a hot path that already need the bytes should reuse `Self::encode` directly instead of pairing `encoded_len` + `encode`." But there's no enforcement — any caller that called `encoded_len` for sizing then `encode` for the bytes pays 2× the encode cost.

**Fix:** Track encoded size on the struct after first encode, cache it. Or expose a `postcard::serialize_len` variant that walks the structure measuring without allocating. Or simply mark `encoded_len` as non-O(1) and have callers reach for `encode().len()` directly when they need the size — at least the cost would be visible.

For workloads that publish many Manifest/Tree blobs (typical for chunked storage), pairing `encoded_len` + `encode` doubles the per-publish work.

### 175. `GreedyRuntime::dispatch_event` takes 3 mutex acquisitions + a full CapabilitySet clone PER EVENT

**Location:** `dataforts/greedy/runtime.rs:704-820`. The greedy cache fires per inbound event for tail-subscribed channels. Per event, steady state:

```rust
let local_caps = self.inner.local_caps.lock().clone();   // <-- mutex + full clone
// ...
let colocation_target_held = self.inner.cache.lock().contains_origin(target);   // <-- mutex 2
// ...
let admitted_by_budget = self.inner.budget.lock().try_consume(...);   // <-- mutex 3
// ...
let cache = self.inner.cache.lock();   // <-- mutex 4 (steady state, line 793)
```

Worst case: 4 mutex acquisitions + a CapabilitySet clone per event. CapabilitySet contains HashSets and BTreeMap — full deep clone allocates.

For a 10K events/sec workload through greedy: 40K mutex ops/sec + 10K full CapabilitySet clones/sec. The clone alone is probably 1-2μs.

**Fix:**
- **`local_caps`:** `ArcSwap<CapabilitySet>` instead of `Mutex<CapabilitySet>`. Reads become one atomic load. Updates (rare) do an Arc swap. Eliminates both the mutex acquire AND the clone.
- **`budget` and `cache`:** if these can be restructured to internal atomics (rate counters, LRU positions), much of the mutex traffic disappears. Cache lookups specifically: per-event `cache.get(channel)` could be a sharded DashMap instead of a single Mutex.

The local_caps fix alone is a clear 30-50% reduction in per-event greedy overhead. Compounds with #149 (same pattern in load balancer metrics).

### 176. `GreedyCacheRegistry::contains_origin` is O(N) linear scan, called per event

**Location:** `dataforts/greedy/cache.rs:206-211`:
```rust
pub fn contains_origin(&self, origin_hash: u64) -> bool {
    if origin_hash == 0 { return false; }
    self.entries.values().any(|e| e.origin_hash == origin_hash)
}
```

Called from `dispatch_event` for the colocation gate. For 1000 cached channels, 1000 u64 compares per event. Doc acknowledges: "O(n) over cached channels — colocation hints are expected to be sparse, but for very large caches a future slice may want a reverse index."

**Fix:** Maintain `origin_index: HashMap<u64, ChannelName>` alongside `entries`. O(1) lookup. Update on insert/remove/eviction. Trades one extra map entry per insert for O(1) hot-path queries.

Only matters if colocation hints are common in the workload. If sparse (as the doc claims), this rarely fires. Worth measuring before fixing.

## 🟡 Medium-impact

### 177. `verify_manifest_chunks` walks chunks twice (once for length, once for hash verify)

**Location:** `blob/dispatch.rs:155, 163-175`:
```rust
let total: u64 = chunks.iter().map(|c| c.size as u64).sum();
if total != fetched.len() as u64 { ... }
let mut offset: usize = 0;
for chunk in chunks.iter() {
    let end = offset + chunk.size as usize;
    let region = &fetched[offset..end];
    let computed: [u8; 32] = blake3::hash(region).into();
    // ...
}
```

First pass computes the sum to validate length; second pass walks again to hash. Could be one pass tracking `running_offset` + early-exit if it overruns.

**Fix:**
```rust
let mut offset: usize = 0;
for chunk in chunks.iter() {
    let end = match offset.checked_add(chunk.size as usize) {
        Some(e) if e <= fetched.len() => e,
        _ => return Err(BlobError::Backend(...))   // length-overrun
    };
    let region = &fetched[offset..end];
    let computed: [u8; 32] = blake3::hash(region).into();
    if computed != chunk.hash { return Err(...) }
    offset = end;
}
if offset != fetched.len() { return Err(...) }   // total-mismatch
```

One pass. Same correctness.

Also: chunk hashing is parallelizable via rayon — independent chunks, no data dependency. Same fix as #173 hashing parallelization.

### 178. `MeshBlobAdapter::fetch` allocates a Vec for the heat-bump hash list

**Location:** `blob/mesh.rs:3080-3088`:
```rust
let hashes: Vec<[u8; 32]> = match blob_ref {
    BlobRef::Small { hash, .. } => vec![*hash],
    BlobRef::Manifest { chunks, .. } => chunks.iter().map(|c| c.hash).collect(),
    BlobRef::Tree { .. } => Vec::new(),
};
self.bump_heat(&hashes);
```

Allocates a Vec per fetch (even for the single-hash Small case via `vec!`). Then bumps each hash inside `bump_heat`.

**Fix:** `bump_heat` takes an iterator instead of a slice. Caller passes an iterator over the appropriate hash source without allocating:
```rust
fn bump_heat(&self, hashes: impl IntoIterator<Item = [u8; 32]>) { ... }

// Caller:
match blob_ref {
    BlobRef::Small { hash, .. } => self.bump_heat(std::iter::once(*hash)),
    BlobRef::Manifest { chunks, .. } => self.bump_heat(chunks.iter().map(|c| c.hash)),
    BlobRef::Tree { .. } => {}
}
```

Per fetch: one Vec allocation eliminated.

### 179. `OverflowController::pick_target` walks every node in the capability index per push

**Location:** `blob/overflow.rs:748-791`. Per overflow tick (potentially per pushed blob within the tick):
```rust
for node_id in self.capability_index.all_nodes() {
    let Some(caps) = self.capability_index.get(node_id) else { continue };
    let peer_blob = BlobCapability::from_capability_set(&caps);
    if !peer_blob.storage || !peer_blob.overflow_enabled { continue }
    // ... more filters ...
}
```

For a 1000-node mesh: 1000 DashMap gets + caps clones + 2 capability struct parses per call.

Tick-cadence, not per-event, but if the overflow tick has many candidates to place, this is called per candidate.

**Fix:** Pre-bucket capability index into "nodes advertising blob storage with overflow enabled" — maintained on capability announcement. Pick_target walks only that subset. For mostly-non-storage meshes, this collapses to a small slice.

### 180. `MeshBlobAdapter::fetch` final assembly grows Vec with `extend_from_slice` per chunk

**Location:** `blob/mesh.rs:3048`. As discussed in #172, intentionally avoids pre-allocation for security reasons (hostile manifest could declare massive size).

The safe pre-alloc pattern: cap by the actual `MAX_BULK_FETCH_BYTES` ceiling (256 MiB) and pre-allocate up to `total_size.min(MAX_BULK_FETCH_BYTES)`. Hostile manifests are bounded by the cap; legitimate fetches get a single allocation.

```rust
let cap = (*total_size).min(MAX_BULK_FETCH_BYTES) as usize;
let mut out: Vec<u8> = Vec::with_capacity(cap);
```

Saves the O(log N) reallocs during assembly.

### 181. `HeatRegistry::evict_lru` is O(N) min scan per eviction

**Location:** `dataforts/gravity/counter.rs:249-258`:
```rust
let victim = self.counters.iter()
    .min_by_key(|(_, c)| c.last_update())
    .map(|(k, _)| *k);
```

Doc acknowledges amortized OK ("runs at most once per `entry_mut` past the cap"). But for workloads with high cardinality of new hashes, every new hash past the cap triggers a full-table scan.

**Fix:** Replace `counters: HashMap<u64, HeatCounter>` with an LRU map (`lru` crate or similar). O(1) eviction. Cost: small extra memory per entry for the linked-list pointers.

Only matters if heat tracking churns through many distinct hashes (cold-data workloads). For workloads with stable hot sets, the cap is rarely hit.

### 182. `HeatCounter::bump` calls `powf` per bump

**Location:** `dataforts/gravity/counter.rs:68`. Per blob fetch's heat bump, a transcendental computation. Per chunk in a multi-chunk fetch.

`powf` is ~20-40 cycles. For a 16-chunk fetch: 16 powf calls per fetch. Per event-rate fetches, 16M powf/sec at 1M chunks/sec. Modest.

**Fix:** For the common case where elapsed is much smaller than half_life (typical for hot data), `0.5_f64.powf(x)` ≈ `1.0 - x * 0.693` for small x. A fast-path that avoids `powf` for small ratios would handle the common case faster.

Alternatively: pre-compute a lookup table of `0.5^x` for fractional x in [0, 1). The integer part of half_lives shifts the mantissa; the fractional part indexes the table.

Probably not worth it unless heat tracking is observed to be a bottleneck. Listed for completeness.

### 183. `chunk_payload` runs sequential BLAKE3 over chunks

**Location:** `blob/blob_ref.rs:991-1001`:
```rust
let mut chunks = Vec::with_capacity(chunk_count);
for slice in bytes.chunks(chunk_size) {
    let hash: [u8; 32] = blake3::hash(slice).into();
    chunks.push((ChunkRef { hash, size: slice.len() as u32 }, slice));
}
```

For a 64MB blob with 64KB chunks: 1024 sequential BLAKE3 calls. At ~3GB/s single-threaded BLAKE3, ~20ms total.

**Fix:** rayon parallel iter:
```rust
use rayon::prelude::*;
let chunks: Vec<_> = bytes
    .par_chunks(chunk_size)
    .map(|slice| (ChunkRef { hash: blake3::hash(slice).into(), size: slice.len() as u32 }, slice))
    .collect();
```

8-core speedup: ~3ms instead of 20ms for a 64MB blob. Matters for store-path latency on large blobs.

Also: BLAKE3's `Hasher::update_rayon` parallelizes within a single hash for very large inputs. For 64KB chunks the per-chunk parallelism wins; for multi-MB chunks both layers could compose.

### 184. `MeshBlobAdapter::fetch_chunk` returns `Vec<u8>` forcing a copy from `Bytes`

**Location:** `blob/mesh.rs:2465`: `let bytes = first.payload.to_vec();`

`first.payload` is `Bytes`. `to_vec()` forces a full copy. The `BlobAdapter::fetch_chunk` API returns `Vec<u8>`, so the copy is at the API boundary.

**Fix:** Change `BlobAdapter::fetch_chunk` (and `fetch`, `fetch_range`) to return `Bytes`. Callers that need `Vec<u8>` can `.into()` it (which is also a copy, but only the callers that genuinely need ownership pay it). Most consumers can work directly with `Bytes` — zero-copy slicing, refcount sharing across the assembly chain.

Same pattern as #84, #128. Architectural fix that propagates through the adapter contract.

For a manifest fetch of N chunks: N×payload_size bytes of memcpy eliminated. For 64MB blobs, 64MB of memcpy per fetch.

## 🟢 Low-impact / cleanup

### 185. `parse_blob_heat_tag` uses `u8::from_str_radix` for hex byte parsing

`blob/migration.rs:119-122`. Hex byte parsing via the general-purpose radix parser. Same pattern as #171's encoding side — fast hex byte decode via lookup table would be cheaper. Tick-cadence path, so low frequency.

### 186. `BlobRefcountTable::pinned_count` and `zero_refcount_count` walk the full table

`blob/refcount.rs:219, 260`. Used for Prometheus metrics. Linear over DashMap. If scrape interval is 15s, walking even a 100K-entry table is fine. Could maintain counters incrementally but probably not worth the bookkeeping.

### 187. `MeshBlobAdapter::store` Manifest path rejects ReedSolomon at v0.2

`blob/mesh.rs:2919-2925`. Returns an error per ReedSolomon store attempt. Cold (error path).

### 188. `chunk_payload` recomputes the chunk size constant

`blob/blob_ref.rs:983`: `let chunk_size = BLOB_CHUNK_SIZE_BYTES as usize;` — const, cheap. Cleanup-grade.

### 189. `BlobRef::decode_small` calls `.to_owned()` on a UTF-8 validated slice

`blob/blob_ref.rs:672-674`. Per Small BlobRef decode: a String allocation for the URI. Unavoidable if the API needs an owned URI. Cleanup-grade.

### 190. `verify_manifest_chunks` returns a `BlobError::Backend(format!(...))` for length mismatch

`blob/dispatch.rs:156-162`. Format allocation per error. Cold (error path).

### 191. `BlobMigrationCandidate` clones the publisher caps

`blob/migration.rs:142`. Tick-cadence path (gravity emit interval). Could be `Arc<CapabilitySet>` reference instead, but the candidates Vec is dropped at end of tick. Cleanup-grade.

---

## What I'd actually do

The dataforts findings split cleanly into "per-event hot" (greedy cache) vs "per-blob-op" (everything else). Treat them differently.

**Per-event greedy cache (do these first):**

1. **#175 — ArcSwap for `local_caps`.** Removes a mutex + full CapabilitySet clone per cached event. Probably 30-50% reduction in greedy `dispatch_event` overhead.

2. **#176 — origin reverse index in GreedyCacheRegistry.** Only if colocation hints are common; profile first.

Together these make the greedy path comparable to the bus's own ingest cost.

**Per-blob-op (do these for blob-heavy workloads):**

3. **#171 — lookup-table hex encoding.** Touches every blob operation. Mechanical fix.

4. **#172 + #173 — parallel chunk I/O on fetch and store.** Easy 5-15× speedup on bulk operations against replicated blobs. Largest absolute latency improvement in the dataforts layer.

5. **#184 — `Bytes` instead of `Vec<u8>` in BlobAdapter trait.** Architectural change but eliminates GB/sec of memcpy on bulk fetches. Propagates through callers; bigger diff but bigger win.

6. **#174 — fix `encoded_len` to not encode-and-discard.** Doubles publish performance for Manifest/Tree blobs if any caller pairs `encoded_len` + `encode`.

7. **#183 — rayon parallel BLAKE3 on chunk_payload.** ~7× speedup on store-path hashing for large blobs.

**Skip unless profiling justifies:**

The migration/overflow/heat tick items (#179, #181, #182, #185) are cold (tick-cadence). Even O(N) work at tick-rate is acceptable for most deployments. Worth revisiting only if the tick is observed to be a bottleneck.

---

## Cross-cutting

The patterns in dataforts mirror prior rounds:

- **`local_caps.lock().clone()` (#175)** is the same anti-pattern as **#11 (RedexIndex)**, **#96 (Memories)**, **#149 (LoadBalancer)**. ArcSwap-and-snapshot fix uniformly applicable.
- **Sequential I/O over independent units (#172, #173)** matches the pattern in **#83 (MemoriesWatcher)** — workloads naturally parallel are run sequentially.
- **`Vec<u8>` instead of `Bytes` at API boundaries (#184)** matches **#84 (RPC body)**, **#128 (decrypt)**, **#51 (HeapSegment::read)**.
- **`.to_string()` / formatter machinery for byte conversion (#171, #185)** matches the recurring "write! per byte" pattern called out across rounds.

A workspace-wide cleanup pass on each of these four patterns would resolve dozens of items across all subsystems with similar diffs.

---

## Honest expectation

The dataforts layer is "hot when active." If blob storage is a real workload:

- **High-rate small-blob workloads** (small documents through `publish_blob`): #171, #184 matter most. Probably 2-3× on the per-blob CPU cost.
- **Large-blob bulk fetches** (manifest blobs serving file-sized payloads): #172, #173, #174 matter most. Likely 5-15× on bulk operation latency for replicated blobs.
- **Cached pub/sub workloads** (greedy cache active on hot channels): #175 matters most. Per-event mutex overhead halved.

If users don't run blob storage at scale, this entire section is dormant. The greedy cache might still fire if any tail-subscription is active — even with no blobs, #175 helps if cached channels are used.

The dataforts subsystem also has the cleanest "architectural" wins in the audit: #184 (Bytes-throughout) is a single trait change that eliminates GB/sec of memcpy across the entire blob layer. Worth doing even if the rest of the items aren't.
