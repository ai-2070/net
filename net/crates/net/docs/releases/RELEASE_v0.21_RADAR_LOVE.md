# Net v0.21 — "Radar Love"

*Named after Golden Earring's 1973 cut — the one with Cesar Zuiderwijk's two-bar drum intro that every garage band on three continents has tried to copy, and George Kooymans' lyric about a driver getting a wordless lover's-distress signal at half past four in the morning and burning it down the highway to answer it. "I been driving all night, my hand's wet on the wheel" — the song's whole urgency is in the gap between the call landing and the driver arriving, and shrinking that gap to as close to zero as the road will allow. v0.19 pushed the substrate past its prior throughput ceilings; v0.20 added a signed authorization gate on top of every nRPC invoke. v0.21 turns the dial toward latency — eliminating dead time on the hot path. Per-packet RX no longer pre-zeroes a 1500-byte buffer just for the kernel to overwrite it. Manifest fetches no longer wait for the previous chunk before requesting the next one. The capability index no longer clones a HashSet to compute an intersection. The replay-window check no longer takes the same lock twice. Across ~100 fixed items the substrate gets a faster reflex arc on every hot path that runs more than once per request.*

## A faster reflex arc on every hot path

The v0.21 release is the result of five back-to-back performance audits across the substrate (`net-perf-analysis.md`), the dataforts compositional layer (`net-dataforts-analysis.md`), the crypto + session + reliability wire-path (`net-crypto-session-reliability-analysis.md`), the discovery + routing surface (`net-discovery-routing-analysis.md`), the compute runtime (`net-compute-analysis.md`), and the dormant federated-query layer (`net-meshdb-analysis.md`). Each audit produced a ranked-by-impact list of "this allocates per event when it doesn't need to" / "this takes a lock per call when it could take none" / "this scans linearly when an index would be O(1)" / "this re-encodes when it could measure" items. v0.21 lands roughly 100 of them — the high-impact ones on every audit, plus the cleanest of the mediums where the fix was small enough to bundle.

The pattern across all five audits is the same: **the substrate had a steady-state shape that worked but paid for it in allocator pressure, lock contention, and memcpy**. Pre-v0.21 a 1M-packet-per-second receive workload spent more time zero-filling a buffer before each `recvmmsg` slot than it spent verifying the AEAD tag on the resulting packet. A 1024-chunk manifest fetch waited for each chunk's HTTP-equivalent round-trip before asking for the next one — a 1 s wall-clock fetch where 64 ms was the actual chunk-service-time minimum. A `LeastLatency` endpoint selection at 100 endpoints did 100 RwLock acquires and 100 `LoadMetrics` deep-clones per event just to read the values used to pick one. None of it was visible at a small scale; all of it bit at the throughput ceiling the v0.19 streaming surface exposed.

The fixes are localized — no architectural rework, no protocol changes, no fold rewrites. The wire format is unchanged; the public substrate API moves on a handful of types where the shape change pays for itself many times over (`Bytes` vs `Vec<u8>` on RPC and blob bodies, `Arc<Batch>` instead of `Batch` on the bus retry path, `Vec<Arc<Memory>>` instead of `Vec<Memory>` on CortEX query returns). Everything else lands under the hood — operators get the wins by bumping the dependency.

Below: the wins, grouped by where they fire.

---

## Transport + crypto: zero-copy RX, half the locks per packet

The per-packet receive path was the single biggest pool of waste in the substrate. v0.21 closes most of it.

**Kill the recv-buffer zero-fill.** `PacketReceiver::recv` used to call `resize(MAX_PACKET_SIZE, 0)` per packet — a ~1500-byte `memset` whose only purpose was to give the kernel a slice to overwrite milliseconds later. The fix is tokio's `recv_buf_from`/`recv_buf`/`try_recv_buf_from`, which write directly into `BufMut` spare capacity without the pre-zero. The same pattern showed up on three sibling `NetSocket` entry points and on Linux's `BatchedTransport::recv_batch` (which zero-filled all 64 batch slots, ~512 KiB of `memset` per batch). At 1 M pps that's around 9 GB/sec of memory bandwidth gone — entirely.

**Zero-copy RX decrypt.** The receive path used to call an allocating `decrypt` that produced a fresh `Vec` per packet. v0.21 adds `PacketCipher::decrypt_to_bytes`, which tries `Bytes::try_into_mut` first (the common UDP refcount-1 case decrypts in place into the inbound buffer's allocation) and falls back to allocation only when the buffer is shared. At 1 M pps with 1 KB packets this saves roughly 1 GB/sec of allocator churn.

**Cached nonce template.** `nonce_from_counter` rebuilt the 12-byte AEAD nonce from scratch (prefix `memcpy` + zero-init + counter write) on every encrypt and decrypt. v0.21 caches the template on the cipher; only the counter bytes get written per call.

**Single-lock RX admit.** The replay-window check used to take two separate `parking_lot::Mutex` acquisitions per inbound packet: one before decrypt (`is_valid_rx_counter`) and one after (`update_rx_counter`). v0.21 collapses them into a single `try_admit_rx_counter`. Replays now pay an AEAD verify before rejection (priced in — replays are rare), and the steady-state path drops 10–20 M lock ops/sec at 1 M pps.

**Verify-only heartbeat path.** Heartbeats used the allocating `decrypt` purely to drop the result (the call was `decrypt(...).is_err()`). v0.21 adds `PacketCipher::verify`, which runs the AEAD tag check via in-place decrypt over a single scratch `BytesMut`. No alloc per heartbeat.

**Arc-shared retransmit descriptors.** `RetransmitDescriptor` carries a `Vec<Bytes>` per outbound chunk-group; the reliability layer used to deep-clone it on every NACK or timeout emission. v0.21 switches the reliability-mode trait to exchange `Arc<RetransmitDescriptor>` — retransmits are one refcount bump regardless of inner Vec length.

---

## Dataforts: parallel chunk I/O, Bytes through the trait, lookup-table hex

The blob fabric grew a 16-wide concurrent fetch and an end-to-end `Bytes` flow.

**Parallel manifest fetch + store.** `MeshBlobAdapter::fetch(Manifest)` used to walk chunks sequentially — for a 1024-chunk replicated blob at 1 ms per chunk, that was a 1-second wall-clock fetch where the actual chunk-service-time minimum was 64 ms. v0.21 wraps the chunk iteration in `stream::iter(...).buffered(16)` (ordered, to preserve assembly correctness). The store path gets the symmetric treatment via `buffer_unordered(16)` — content-addressed writes are order-independent and idempotent — with a hoisted verification prepass so the "no chunks stored on a caller-poisoned manifest" contract still holds. Roughly a 15× latency reduction on bulk manifest operations.

**`Bytes` through the BlobAdapter trait.** `BlobAdapter::fetch` / `fetch_range` / `MeshBlobAdapter::fetch_chunk` returned `Vec<u8>`, which forced a `.to_vec()` memcpy of the chunk's `Bytes` payload at every layer boundary. v0.21 switches the trait surface across every implementor (Rust, FFI, Python, Node) to `Bytes`. The blob-tree node cache also moves to `Bytes` — cache hits become Arc clones rather than `Vec::clone`. On bulk-fetch workloads this saves gigabytes per second of `memcpy`.

**Lookup-table hex encoding.** `hex32` and `chunk_channel` used to call `write!("{:02x}", b)` 32 times per blob op. v0.21 adds a `HEX_LOWER` lookup table + zero-alloc `hex32_into(&[u8; 32], &mut [u8; 64])` — roughly 10× faster, and zero allocation. `parse_blob_heat_tag` got the symmetric treatment on the decode side via a nibble lookup table.

**Single-pass manifest verification.** `verify_manifest_chunks` used to walk the chunk list twice (a sum-check pass for total-size validation, then a hash-check pass for content validation). v0.21 fuses them into a single pass with `checked_add` for overflow and an `end > fetched.len()` bounds-check before each slice.

**Measure, don't re-encode.** `BlobRef::encoded_len` used to do a full re-encode for `Manifest` and `Tree` variants — allocate a `Vec`, postcard-serialize, read `.len()`, drop. The common pairing of `encoded_len` + `encode` was paying the encode cost twice. v0.21 switches `encoded_len` to `postcard::experimental::serialized_size`, which walks the type tallying bytes without allocating.

**Greedy + heat micro-wins.** `GreedyCacheRegistry::contains_origin` was O(N), called per admission carrying a colocation hint; v0.21 adds an `origin_counts: HashMap<u64, usize>` reverse index for O(1) lookups. `GreedyRuntime::local_caps` was `Mutex<Arc<CapabilitySet>>` cloned per `dispatch_event`; v0.21 switches to `ArcSwap` — reads become one lock-free Acquire load. Post-fetch heat-bump used to build a `Vec<[u8; 32]>` of hashes per call; `bump_heat` now takes `impl IntoIterator<Item = [u8; 32]>` and streams directly. Manifest-assembly Vec is pre-allocated to `total_size.min(MAX_BULK_FETCH_BYTES)` (256 MiB cap protects against hostile manifests).

---

## Discovery + routing: single-pass filters, cached resolution, smaller dedup

The capability surface and the publish path picked up a clutch of localized wins where the per-call work was steady-state O(N) on values that didn't need to be.

**Cached session NodeId.** `dispatch_packet` used to resolve session → NodeId per inbound packet, which meant two `DashMap` lookups and a possible O(N) peer scan when the source address had drifted. v0.21 caches the resolved id on `NetSession` as `cached_node_id: AtomicU64` (sentinel 0 = unresolved); the fast path is one Relaxed load.

**Arc-shared publish events.** `publish_many` used to clone the events `Vec<Bytes>` per spawned task. For 100 subscribers × 1000 events: 100 Vec allocs + 100 K Bytes refcount bumps. v0.21 hoists into `Arc<[Bytes]>` — 1 Vec alloc + 1 K Bytes bumps + 100 Arc bumps.

**Single-pass subscriber filter.** `publish` used to do two sequential `retain` passes over the subscriber Vec (subnet visibility, then auth/token). v0.21 fuses them into one retain closure with the cheapest check first — single walk over the Vec.

**Vec-based capability intersection.** `CapabilityIndex::build_candidate_set` used to clone full `HashSet`s to intersect them — one `HashSet` alloc per indexed filter clause. v0.21 switches the working set to `Vec<u64>`: the first match materializes one Vec, subsequent clauses use in-place `retain(|n| index.contains(n))` — no new container per clause.

**Threshold-based dedup HashSet.** `dispatch_recipients` did O(N) `out.contains(&picked)` for dedup. v0.21 keeps the linear scan as the fast path and promotes to `HashSet<u64>` only once `out` crosses a 32-entry threshold — the common small-recipient-set case stays branch-predictor-friendly, the large case stops being O(N²).

**Single-pass scoped find.** `find_nodes_scoped` used to run the filter then re-iterate doing `get(node_id)` per survivor (full `CapabilitySet` clone per node) just to read `caps.tags`. v0.21 folds scope resolution into the same shard-lock guard as filter re-validation — zero `CapabilitySet` clones.

---

## Compute + load balance: lock-free metrics, O(1) reverse indexes

The compute runtime gets its hot paths the same treatment as the wire path.

**Lock-free LoadMetrics.** `EndpointState::metrics()` used to do a RwLock read plus a 9-field clone per call — per endpoint per select. At 100 endpoints with `LeastLatency`, that was 100 RwLock acquires + 100 deep clones per event. v0.21 switches to `ArcSwap<LoadMetrics>` and adds a `load_score()` helper that reads via guard; the metrics fields stay private but the score is one indirection.

**`max_by`, not sort.** `Scheduler::pick_best_candidate` was doing an O(N log N) `sort_by` only to take the first element. v0.21 switches to O(N) `max_by` with inverted tie-break direction.

**Reverse index in the group coordinator.** `origin_hash_for_entity_id` was a linear scan comparing 32-byte NodeIds. For a 100-member group at 100 K ev/s that meant 10 M × 32-byte comparisons/sec. v0.21 adds `origin_hash_by_entity_id: HashMap<NodeId, u64>` — lookup is O(1).

**Skip horizon encode on empty outputs.** `DaemonHost::deliver` was calling `horizon.encode()` (walks the horizon map + xxh3 per entry) per event even when the daemon produced no outputs. v0.21 early-returns when `outputs.is_empty()` after observation accounting.

**Streaming parent-hash.** `compute_parent_hash` used to allocate a Vec, `memcpy` the 32-byte link + payload in, hash, drop. At 100 K ev/s with 1 KB payloads: 100 K allocs/sec + 100 MB/sec `memcpy`. v0.21 switches to streaming `Xxh3::update` — zero alloc.

**AtomicU64 last_selected.** `EndpointState::last_selected` was `Mutex<Instant>` used purely as cell storage. v0.21 switches to `AtomicU64` of nanos since a `OnceLock<Instant>` baseline. For 100 K successful selections/sec: 100 K lock+unlock pairs gone.

**O(1) member-index lookup.** `mark_healthy` / `mark_unhealthy` / `update_member_placement` used to do a linear `iter_mut().find(|m| m.index == index)`. v0.21 switches to `members.get_mut(index as usize)` with a defensive re-check — O(1) with the same correctness invariant.

---

## Core bus + RedEX + RPC: zero-copy reads, Bytes payloads, Arc-shared batches

The substrate's per-event spine — the bus, the append-only log, the RPC payloads — gets the same treatment.

**Arc-shared batch on retry.** `dispatch_batch` used to clone the entire `Batch` on every retry attempt, including attempt 0. v0.21 switches `Adapter::on_batch` to take `Arc<Batch>` — retries are now a refcount bump. For a 1000-event batch with retries, this saves 1000+ Bytes refcount bumps and one Vec alloc per dispatch.

**ArcSwap shard selection.** `Mapper::select_shard` used to do two Vec allocs + a RwLock read per event in dynamic mode. v0.21 pre-computes the selection into `ArcSwap<SelectionTable>` and reads via guard. At 10 M ev/s this removes 20 M allocs/sec.

**Amortized TLS pool reaping.** `ThreadLocalPool::acquire`/`release` used to run a HashMap retain + `Weak::strong_count` walk on every call. v0.21 amortizes to every 4096th call via a per-thread counter. The published "thread-local 2× slower than shared" benchmark anomaly should erase.

**Zero-copy `HeapSegment::read`.** Reads used to do `Bytes::copy_from_slice` per call. v0.21 switches the internal buffer to `Bytes`; reads are refcount slices, appends use `Bytes::try_into_mut`. At 4 KB payloads × 100 K ev/s watcher load, this is 400 MB/sec of pure `memcpy` gone.

**Binary search `read_one` / `read_range`.** `RedexFile::read_one` and `read_range` were linear scans over a sorted index. v0.21 switches to `partition_point` — O(N) → O(log N).

**Compiled consumer filter.** Filtered poll used to re-split the path string and re-parse indices per event. v0.21 adds `CompiledFilter`, which runs the path-split + integer-parse once per poll.

**`Bytes`-based RPC payloads.** `RpcRequestPayload` / `RpcRequestChunkPayload` / `RpcResponsePayload::body` was `Vec<u8>`, which forced a `.to_vec()` memcpy per frame decode. v0.21 switches the body field to `Bytes` end-to-end across substrate + Node / Python / Go FFI boundaries. At 100 K RPCs/sec with 1 KB bodies: 100+ MB/sec of memcpy gone.

**In-place router forward.** `Router::route_packet` used to allocate a fresh buffer and full-copy the body just to flip an 18-byte header. v0.21 adds `RoutingHeader::write_at` and uses the `Bytes::try_into_mut` fast path for sole-owned UDP packets, with allocate-and-copy as the fallback.

**Lemire shard hash.** `select_shard_by_hash` in static mode used `hash % n` (~20–25 cycles per event). v0.21 switches to Lemire multiply-shift reduction (~3 cycles) — ~7× cycle reduction on a per-event hot path.

**Inline hot byte codecs.** `RedexEntry::to_bytes` / `from_bytes` and `EventMeta::to_bytes` / `from_bytes` weren't marked `#[inline]`; v0.21 marks them `#[inline(always)]` — the codec is small enough that inlining lets the compiler erase the wrapper-call overhead across every event-handling site.

**Redis adapter micro-wins.** `redis::serialize_event` used `to_string().as_bytes()` for u64/u16; v0.21 uses `write!` into the existing Vec — two String allocs per event gone. `parse_xrange_response` was setting `last_seen_id` (String alloc) on every iteration even though only the last mattered; v0.21 tracks `last_seen_idx: Option<usize>` and materializes once — 9999 wasted allocs per 10 K-entry response gone. `is_transient_error` used `to_string().to_uppercase()` per classified error; v0.21 uses zero-alloc `RedisError::detail()` + `starts_with`.

---

## CortEX content + title search: ASCII fast path

CortEX's `MemoriesQuery::matches` and `TasksFilterSpec::matches` used `to_lowercase().contains()` per row — a full Unicode case-folding pass on the entire content body, then a `contains` walk, per search predicate per row. v0.21 adds `ContentNeedle` and `TitleNeedle` wrappers with an ASCII fast path: when the needle is pure ASCII (the overwhelming common case), the match runs as `eq_ignore_ascii_case` byte scan with zero allocation. Non-ASCII needles fall back to the existing Unicode path. For 100 K memories at 4 KiB each, this eliminates roughly 400 MB of allocation and 400 MB of case-folding per content search.

**Arc-shared memory state.** `MemoriesState` used to store `Memory` by value, which meant query/watcher returns deep-cloned every Memory the caller observed. v0.21 switches the store to `Arc<Memory>`; query and watcher APIs return `Vec<Arc<Memory>>`; writers use `Arc::make_mut` for copy-on-write semantics; the FFI surface uses `Arc::try_unwrap` to avoid double-clone on the unwrap path.

---

## MeshDB: pre-activation hardening for the dormant federated layer

The federated-query layer is gated behind the `meshdb` Cargo feature and currently dormant in production. v0.21 lands the pre-activation hardening pass so the layer is ready when the feature flips on.

- **Parallel hash-join sub-fetches.** `tokio::try_join!` around the two halves of a federated hash-join: pure 2× on every remote/remote join (50 ms RTT each: 100 ms → 50 ms wall).
- **DashMap caller-side inflight.** Caller-side inflight map used to be `Arc<RwLock<HashMap>>`; every send took a write lock and concurrent sends to distinct call_ids serialized. v0.21 switches to `Arc<DashMap>` — sharded per call_id.
- **Cached `approx_bytes` on `CachedResult`.** Walked every row per LRU bookkeeping call; v0.21 caches at construction in a private `u64` field — single field load.
- **Pre-sized `drain_rows`.** Switched from `Vec::new()` + grow-by-doubling to `Vec::with_capacity(DRAIN_INITIAL_CAPACITY)`.
- **Lookup-table planner `chain_hex`.** `format!("{:016x}", origin_hash)` replaced with a `HEX_NIBBLES` 16-shift unroll into a 16-byte stack buffer.

---

## Architecture work: subnet spec + multifold plan land as design docs

Two architectural design documents land alongside the perf work, both targeting future releases:

- **`SCALING_SUBNET_SPEC.md`** — formal specification of how the substrate carves an arbitrarily-large mesh into operator-defined subnets, the membership protocol, the cross-subnet routing primitives, and the auth model that interacts with the v0.20 capability-auth `SubnetId` allow-list axis. Design doc only in v0.21; the implementation tracks separately.
- **`SCALING_MULTIFOLD_PLAN.md`** — plan for parallelizing the substrate's fold (consume-events-into-state) layer across multiple shards per stream rather than the current single-fold-per-stream model. Same — design only in v0.21.

Operators tracking the substrate's scaling roadmap should read both; neither is wired into the runtime yet.

---

## Test hygiene

- **Lib suite at 3950+ tests** (was 3850+ at v0.20.2 release). 100+ net new tests across the perf-pinning regressions — zero-copy reads pinned against accidental `Bytes::copy_from_slice` reintroduction, `ArcSwap` selection table pinned against ABA on concurrent rebuilds, ASCII fast-path pinned to fall back correctly on non-ASCII needles, single-lock RX admit pinned against replay-after-AEAD-verify ordering, parallel manifest fetch pinned against ordering-vs-correctness, and the surface tests on the new public API (`Bytes`-returning blob fetches, `Arc<Memory>`-returning CortEX queries, `Arc<Batch>`-accepting bus adapters).
- **`cargo clippy --features meshos,deck --all-features --all-targets -- -D warnings` clean.** The strict floor from v0.20.2 (`unwrap_used`, `expect_used`, `undocumented_unsafe_blocks`, `multiple_unsafe_ops_per_block`) stays armed.
- **`cargo doc --features meshos,deck --no-deps` clean under `RUSTDOCFLAGS="-D warnings"`.** Doc-comment hygiene includes a sweep of bracketed perf-doc refs that rustdoc was misinterpreting as broken intra-doc links.
- **Codecov coverage** sits at ~90% on the substrate feature set, informational on the CI status — same posture as v0.20.2.

---

## Breaking changes

### `Adapter::on_batch` signature

`Adapter::on_batch(&self, batch: Batch)` → `Adapter::on_batch(&self, batch: Arc<Batch>)`. Retries are now a refcount bump on the same batch. Adapters that need ownership of the batch can use `Arc::try_unwrap(batch).unwrap_or_else(|b| (*b).clone())` — falls into the clone branch only when a retry is actually in flight against the same batch.

### `Mesh::send_to_peer` / `Mesh::send_routed` take `&Batch`

Both used to take `Batch` by value, which forced callers to clone-or-move. v0.21 takes `&Batch` — callers retain ownership and can re-send without re-cloning.

### `Rpc{Request,Response}{,Chunk}Payload.body` is `bytes::Bytes`

The body field on every nRPC payload moves from `Vec<u8>` to `bytes::Bytes`. Constructors: `Bytes::from(vec)`, `Bytes::from_static(b"...")`, `Bytes::copy_from_slice(&buf)`. Accessors: `as_ref()` for `&[u8]`. The Node, Python, and Go FFI bindings wrap at the boundary so binding consumers are unaffected; Rust consumers update construction sites.

### `BlobAdapter::fetch` / `fetch_range` / `resolve_payload` return `bytes::Bytes`

The blob trait surface returns `Bytes` instead of `Vec<u8>` across every implementor (Rust, FFI, Python, Node). Use `as_ref()` for `&[u8]`; call `to_vec()` only when you genuinely need an owned `Vec`. Small-range reads also use `Bytes::slice` internally — repeated range fetches over the same blob share a backing allocation.

### CortEX `MemoriesState` query / watch returns `Vec<Arc<Memory>>`

Memory queries and watcher streams used to return `Vec<Memory>` (deep-cloning every observation). v0.21 returns `Vec<Arc<Memory>>`. Treat as read-only by default; call `Arc::try_unwrap` (or clone the inner `Memory`) when you need an owned mutable copy. Writers use `Arc::make_mut` for copy-on-write semantics, so observed handles remain stable.

### `bump_heat` signature

`bump_heat(hashes: &[[u8; 32]])` → `bump_heat<I: IntoIterator<Item = [u8; 32]>>(hashes: I)`. Callers that previously built a Vec to pass in can now stream directly (`iter::once`, `chunks.iter().map(|c| c.hash)`).

### Removed `static` shard hash via `%`

`select_shard_by_hash` static mode no longer does `hash % n` internally — it uses Lemire multiply-shift. The function signature is unchanged; the change is observable only via cycle count.

---

## How to upgrade

1. **Rust consumers — update the dependency to `0.21`.** Most of the wins land transparently. The breaking changes above are mechanical and the compiler points at every site.

2. **Adapter implementations — switch to `Arc<Batch>`.** If your `Adapter::on_batch` mutates the batch, replace `batch.clone()` with `Arc::try_unwrap(batch).unwrap_or_else(|b| (*b).clone())`. If your `on_batch` is read-only, drop the leading clone entirely.

3. **nRPC callers / handlers — construct payloads with `Bytes`.** Anywhere you built a `RpcRequestPayload` with a `Vec<u8>` body, swap to `Bytes::from(vec)` (no allocation, takes ownership) or `Bytes::copy_from_slice(&slice)` (one allocation for the new buffer). Handlers reading the body: `payload.body.as_ref()` for `&[u8]`.

4. **Blob consumers — handle `Bytes`.** `BlobAdapter::fetch` and friends now return `Bytes`. Most call sites change from `fetch(...).await?` (returning `Vec<u8>`) to `fetch(...).await?` (returning `Bytes`); downstream `&[u8]` access uses `.as_ref()`. Sites that genuinely need an owned `Vec<u8>` call `.to_vec()`, but this is rarely the right answer — `Bytes` is cheaper to pass around.

5. **CortEX consumers — `Arc<Memory>` handles.** Query and watcher returns now carry `Arc<Memory>`. Reads work unchanged via deref coercion (`memory.title`, `memory.content`); mutation requires `Arc::try_unwrap` (succeeds when the Arc is sole-owned) or an explicit clone of the inner `Memory`.

6. **No CI config change required.** The strict clippy floor from v0.20.2 is still the floor; the test-side allow-list is unchanged.

7. **Operators — bump the binary.** Pre-built `net-mesh` and `net-deck` binaries land in the release archive for every supported target (Linux x86_64 / aarch64, macOS x86_64 / aarch64, Windows x86_64). Drop in `/usr/local/bin` (or your platform's equivalent) and restart the daemon. Wire format is unchanged from v0.20.x; mixed-version fleets handshake cleanly.

8. **Operators tracking the scaling roadmap — read the new design docs.** `SCALING_SUBNET_SPEC.md` and `SCALING_MULTIFOLD_PLAN.md` live under `docs/`. Neither is implemented in v0.21; both target future minor releases. Comments and pushback welcome before the implementation lands.

---

Released 2026-05-22.

## License

See [LICENSE](../../LICENSE-APACHE).
