# Performance Audit — Full-Crate Sweep (2026-06-10)

Source: six parallel code-inspection passes over the whole crate, one per subsystem —
core bus (`src/bus.rs`, `src/shard/`, `src/consumer/`, `src/timestamp.rs`), mesh
transport datapath (`src/adapter/net/{mesh,session,crypto,transport,protocol,batch,linux}.rs`),
routing/nRPC/reliability (`src/adapter/net/{mesh_rpc,router,reliability,pool,swarm,route,
reroute,failure,proxy,cancel_registry,mesh_rpc_metrics}.rs` + `channel/`), behavior/
capability folds (`src/adapter/net/behavior/`), RedEX/CortEX/state
(`src/adapter/net/{redex,cortex,state,netdb}/`), and the Dataforts blob layer
(`src/adapter/net/dataforts/`). Benches reviewed per subsystem to establish what is
currently measured. Per-item costs marked *est.* are reasoned from the code, not
re-measured.

> **Status: findings only (2026-06-10).** No fixes applied. Prior-audit fixes were
> verified as landed before flagging anything (perf #17/#18/#32/#51/#52/#70/#72/#84/
> #96/#100/#128/#132/#138/#171/#172/#184, the May-28 capability fixes, the CDC no-cut
> cache, T1.1 grant coalescing, T2.2 `encode_into`). Everything below is residue those
> audits did not cover — `PERF_AUDIT_2026_06_09_HOT_PATH.md` §12–§14 declared
> behavior/ and dataforts clean at the *control-plane* level; this sweep traced the
> per-candidate / per-packet **query-side** and **store-side** paths and found the
> items in §4 and §6.

**Headline conclusions:**

1. **The fixes often already exist in-tree.** The sendmmsg drain loop, the
   grant-coalescing drainer, the `try_into_mut` in-place header rewrite, the BTreeMap
   LRU index, and the origin-cache `Mutex<LruCache>` pattern all exist — they just
   were not applied everywhere they fit. Several findings below are literally "port
   the fix from the sibling file."
2. **CPU-heavy work runs on the async executor.** Dataforts BLAKE3 hashing and
   Reed-Solomon encoding (multi-MiB, multi-ms) and the RedEX replica-side fsync all
   run inline on tokio workers; there is no `spawn_blocking` anywhere in
   `dataforts/mesh.rs`.
3. **Bench blind spots hide the biggest items.** The ingestion benches drive
   `ShardManager` directly and bypass `EventBus` (§1.1–§1.2 invisible); the mesh
   benches measure isolated pieces but never `dispatch_packet` → egress end-to-end
   (§2.1–§2.3 invisible); nothing benches replication catch-up (§5.1 invisible).

Constraint honored throughout: capability checks routing through fold indexes at
~40 ns (vs sub-ns direct) is **by design** — the fold index scales to millions of
nodes. None of the §4 findings dispute that; they target re-parsing and re-allocation
*around* the index, not the index itself.

Recommended order of attack is at the bottom.

---

## §1 Core bus (`bus.rs`, `shard/`, `consumer/`, `event.rs`, `ffi/`)

Benches measure `ShardManager::ingest/ingest_raw`, `TimestampGenerator::next`,
`InternalEvent` creation, `pop_batch` — **the `EventBus` layer is never benched**,
which is exactly where §1.1–§1.2 live.

### 1.1 Global SeqCst `in_flight_ingests` counter ping-pongs one cache line per event across all producers

- **Where:** `bus.rs:909-917` (`try_enter_ingest`) + `bus.rs:158-164`.
- **Now:** every `ingest`/`ingest_raw` does `fetch_add(1, SeqCst)` + `shutdown.load(SeqCst)`,
  and `IngestGuard::drop` does `fetch_sub(1, SeqCst)` — 2 SeqCst RMWs + 1 SeqCst load
  per event on a single shared cache line.
- **Cost:** everything else on the path is sharded (per-shard mutex, per-shard
  timestamp gen, ArcSwap routing); this one atomic re-serializes all producers.
  Contended RMWs are ~40–100 ns *est.* — can rival the whole shard-path cost and caps
  multi-producer scaling. Negligible single-threaded.
- **Fix:** stripe the counter — fixed array of 16–32 `CachePadded<AtomicU64>` slots
  indexed by a per-thread slot id; the only reader is `shutdown()`'s wait-for-zero
  loop (cold), which sums slots. `ingest_raw_batch` already amortizes the guard.
- **Impact: high (multi-producer). Confidence: high on mechanism, medium on %.**

### 1.2 Per-event global `EventBusStats` counters duplicate per-shard counters that already exist

- **Where:** `bus.rs:933-946, 959-978`.
- **Now:** every ingest also does `stats.events_ingested.fetch_add(1, Relaxed)` on a
  globally shared struct — in addition to `Shard::try_push_raw` already incrementing
  per-shard `ShardCounters` (`shard/mod.rs:153-166`). The 4 `AtomicU64`s share 1–2
  cache lines and false-share with batch workers bumping `events_dispatched`.
- **Fix:** derive bus totals from the lock-free `ShardManager::stats()` aggregation
  (`shard/mod.rs:777-788`) + the existing `events_unrouted` counter; `flush()`'s
  Phase-2 barrier (`bus.rs:1174`) can read the aggregate the same way. Minimum:
  `CachePadded` + striping.
- **Impact: medium (multi-producer). Confidence: high mechanism, medium magnitude.**

### 1.3 Dynamic-scaling metrics add 2× `Instant::now()` + 3 atomic RMWs per event inside the shard mutex

- **Where:** `shard/mod.rs:147-200` + `shard/mapper.rs:165-195`.
- **Now:** with a metrics collector attached (any bus with `config.scaling`), every
  `try_push_raw` takes `Instant::now()` twice (QPC, ~15–30 ns each on Windows) and
  `record_push` does 1 `fetch_add` + 1 `fetch_update` CAS loop + 1 `fetch_add` —
  all while holding the shard mutex other producers wait on. ~60–120 ns *est.* extra
  per event in dynamic mode.
- **Fix:** (a) sample latency 1-in-64 (the scaling heuristic needs an average);
  (b) use the shard's quanta clock (`TimestampGenerator::now_raw`, ~1–5 ns) instead
  of `Instant::now()`; (c) sample `record_buffer_len` too.
- **Impact: medium (dynamic-scaling deployments only). Confidence: high.**

### 1.4 Batch worker locks the producer-hot shard mutex per dispatched batch just to bump an atomic

- **Where:** `bus.rs:2280-2282, 2335-2337`.
- **Now:** after every dispatch: `shard_ref.lock().record_batch_dispatch()` — but
  that method only does a Relaxed `fetch_add` on `ShardCounters`, which lives in the
  parallel lock-free `counters` vec (`shard/mod.rs:316-317`) precisely so stats
  don't need the mutex. The lock acquisition injects periodic latency spikes into
  the ingest path for nothing.
- **Fix:** `ShardManager::record_batch_dispatch(shard_id)` →
  `table.counters[idx].batches_dispatched.fetch_add(1, Relaxed)`, lock-free.
- **Impact: low-medium (per-batch, but perturbs the per-event lock). Confidence: high.**

### 1.5 Dynamic-mode shard-id → index resolution uses std SipHash `HashMap<u16, usize>` per event

- **Where:** `shard/mod.rs:505-513` (with `:319`).
- **Now:** `table.shard_index.get(&shard_id)` — SipHash of a 2-byte key + probe,
  ~15–25 ns *est.*, once per event (`ingest`, `ingest_raw`) and once per event inside
  `ingest_raw_batch`'s bucketing loop.
- **Fix:** direct-indexed lookup table (`shard_id → idx`, sized to max allocated id;
  rebuilt only on scale events), or at minimum FxHash.
- **Impact: low-medium (dynamic mode only). Confidence: high.**

### 1.6 FFI poll path parses every event into a `serde_json::Value` tree and re-serializes the whole response

- **Where:** `ffi/mod.rs:1136-1160`.
- **Now:** for each consumed event, `e.parse()` builds a full JSON DOM, then the
  envelope is rebuilt with `serde_json::to_string(&json!({...}))` — parse → tree
  alloc → re-serialize per event, on bytes that are already valid JSON
  (`StoredEvent.raw`).
- **Cost:** dominates per-event consume cost for FFI/SDK clients (~1–5 µs/KB + allocs,
  *est.*).
- **Fix:** splice raw bytes through — `from_str::<&RawValue>` (zero-copy, validates,
  no tree) and serialize a struct with `events: Vec<&RawValue>`; keep the
  parse-failure fallback on the rare path.
- **Impact: medium-high for FFI consume throughput. Confidence: high.**

### 1.7 `ingest_raw_batch` allocates one growth-by-doubling Vec per shard per call

- **Where:** `shard/mod.rs:646-704`.
- **Now:** `groups: Vec<Vec<Bytes>>` all start at capacity 0; `shards + 1` allocations
  + ~2× redundant memmove of the 32-byte handles per batch.
- **Fix:** `with_capacity(events.len() / nshards * 2)` per group, or flat
  `Vec<(idx, Bytes)>` + counting sort.
- **Impact: low. Confidence: high mechanism, low magnitude.**

### 1.8 `StoredEvent::Serialize` allocates a String copy and re-validates JSON per event

- **Where:** `event.rs:547-577`.
- **Now:** `RawValue::from_string(raw_str.to_string())` — one String copy + a full
  validation parse per serialized event. No in-crate production path serializes
  `StoredEvent` today (FFI builds its own envelope), but any SDK/HTTP layer that
  serializes `ConsumeResponse.events` pays it.
- **Fix:** `serde_json::from_str::<&RawValue>(raw_str)` borrows (still validates, no
  alloc).
- **Impact: low now, medium if a serializing consumer appears. Confidence: high cost, low hotness.**

### 1.9 (By-design, noted for completeness) Drain workers poll at 100 µs

- **Where:** `bus.rs:2484-2497`.
- 16 shards → ~160 k idle wakeups/s. Documented as a deliberate latency-first
  tradeoff. If idle CPU matters: a `tokio::sync::Notify` nudge fired only on the
  empty→non-empty transition eliminates idle polling at ~zero hot-path cost.

**Verified clean:** `shard/ring_buffer.rs` (lock-free SPSC, cache-padded cursors,
single tail publish), `timestamp.rs` (TSC, single CAS), routing (xxh3 cached on
`RawEvent`, Lemire multiply-shift, ArcSwap tables), `consumer/filter.rs` (filters
pre-compiled per poll), `consumer/merge.rs` poll path, `dispatch_batch` retry path
(shared `Arc<Batch>`).

---

## §2 Mesh transport datapath (`mesh.rs`, `session.rs`, `crypto.rs`, `transport.rs`, `protocol.rs`, `linux.rs`, `pool.rs`)

Verified clean first (do not re-fix): in-place AEAD decrypt on refcount-1 buffers
(perf #128), single-lock replay admit (#132), nonce template (#138), no recv-buffer
pre-zeroing (pinned by tests), thread-local builder pools with amortized reap
(#17/#32), grant-coalescing drainer (T1.1), hand-rolled 68-byte header parse (no
serde on the datapath), router sendmmsg drain with reusable `BatchedTransport`,
64 MB socket buffers.

### 2.1 Default send path is one syscall per packet — the sendmmsg loop exists but only `scheduled` streams use it

- **Where:** `mesh.rs:11001-11007` (`deliver_stream_packet`), `mesh.rs:8596-8599`
  (`publish_to_peer`), `mesh.rs:5677-5704` (`send_to_peer`), `mesh.rs:5293+`
  (grant drainer).
- **Now:** the receive side has feature-gated `recvmmsg` batching; the send
  equivalent exists (`router.rs:793-938` — one reusable `BatchedTransport`, groups
  by dest, one `sendmmsg` per peer per drain) but is fed only by the FairScheduler,
  and `StreamConfig::scheduled` defaults to `false` (`stream.rs:120`). Every
  nRPC/publish/event/control packet takes `socket.send_to(...).await` — one
  readiness poll + one syscall per datagram.
- **Cost:** ~1–2 µs *est.* per packet; under burst load this is the dominant TX cost
  and is exactly what the recv side already eliminated.
- **Fix:** opportunistically route direct sends through the existing scheduler/drain
  loop when a backlog exists, or add a small per-socket egress aggregator that
  collects packets produced within the same poll and flushes via `send_batch`.
- **Also:** `linux.rs:183-188` — `send_batch` rejects IPv6, so IPv6 peers silently
  fall back to per-packet sends even inside the router loop.
- **Impact: high. Confidence: high (mechanics verified; win size depends on pps).**

### 2.2 One ~8 KB heap allocation per inbound packet, on both ingress paths

- **Where:** `linux.rs:404-417` (and `:504-518`); `transport.rs:396-401`.
- **Now:** batched path: per packet, `mem::replace(&mut self.recv_buffers[i],
  BytesMut::with_capacity(MAX_PACKET_SIZE))` — a fresh 8 KB malloc per packet plus a
  Vec per batch. Default path: `recv_buf.split().freeze()` hands out a `Bytes` that
  stays alive downstream (decrypted event slices sit in the shard `SegQueue`), so
  the subsequent `reserve(MAX_PACKET_SIZE)` cannot reclaim and allocates a fresh
  8 KB block per packet in steady state — despite the doc comment claiming reuse.
- **Cost:** one 8 KB alloc+free per packet (~50–100 ns *est.*, worse cross-thread),
  plus memory amplification: a 100-byte event pins the full 8 KB region until the
  consumer drains.
- **Fix:** slab-per-batch (recvmmsg into one large refcounted buffer, hand out
  `Bytes` slices per slot), or copy small packets (< ~512 B) into right-sized
  buffers at ingress, or a recv-buffer pool.
- **Impact: medium-high. Confidence: high.**

### 2.3 Per-event `String` event-id formatting on the hottest delivery path

- **Where:** `mesh.rs:5213-5216`.
- **Now:** `String::with_capacity(24)` + `write!(event_id, "{}:{}", seq, i)` +
  `StoredEvent::new(event_id, ...)` per delivered event (`StoredEvent.id: String`,
  `event.rs:456`); fires N times per multi-event packet.
- **Fix:** carry `(seq, idx)` as integers in `StoredEvent` (or a 24-byte inline
  buffer / lazy Display), formatting only when an adapter needs the string.
- **Impact: medium at high event rates. Confidence: high.**

### 2.4 Routed packets destined for this node do an O(peers) linear scan per packet

- **Where:** `mesh.rs:3679-3687`.
- **Now:** `peers.iter().find(|e| e.value().session.session_id() == session_id)` per
  routed-local data packet. The direct path has an O(1) `addr_to_node` fast path
  with the scan only as fallback (`:3751-3763`); the routed path always scans — and
  `dispatch_packet` runs on the single receive task, so this serializes ingress.
- **Fix:** `session_id → node_id` reverse index (same pattern as
  `origin_hash_to_node`), updated at handshake/teardown.
- **Impact: medium (grows with peer count on relay-heavy topologies). Confidence: high.**

### 2.5 Relay forwarding: full-packet copy + `tokio::spawn` per forwarded packet

- **Where:** `mesh.rs:3705-3714`.
- **Now:** for not-for-us routed packets: new `BytesMut`, copy routing header +
  entire payload (just to apply `fwd_header.forward()`, a hop-count bump), then a
  fresh tokio task per packet for one `send_to`. Per packet: full-datagram memcpy +
  alloc + task spawn (~0.5–1 µs *est.*) + syscall.
- **Fix:** the inbound `Bytes` refcount is 1 here — `data.try_into_mut()` and patch
  the few header bytes in place (router.rs perf #18 is the exact precedent); send
  without spawning, or push to the router scheduler to also gain §2.1's batching.
- **Impact: medium-high for relay nodes. Confidence: high.**

### 2.6 `PacketBuilder` pays an avoidable full-payload memcpy per built packet

- **Where:** `pool.rs:216-220` (and `:292-294` in `build_subprotocol`).
- **Now:** events are framed into `self.payload`, encrypted in place there, then the
  whole encrypted payload is copied again into `self.packet` after the header
  (`extend_from_slice` × 2). Because `packet.split().freeze()` hands the allocation
  away, the packet buffer also re-allocs amortized ~1 per few packets.
- **Fix:** reserve `HEADER_SIZE` bytes in `packet`, frame events directly at offset
  68, `encrypt_in_place` on that sub-slice, patch the header bytes — eliminates the
  second copy of up to ~8 KB per packet on every TX.
- **Impact: medium. Confidence: high.**

### 2.7 Two `SystemTime::now()` calls per packet, each direction

- **Where:** `mod.rs:232-237` (`current_timestamp`); RX: `stream.update_rx_seq` →
  `touch()` (`session.rs:1444-1447`) + `session.touch()` (`mesh.rs:3792`); TX:
  `next_tx_seq()` → `touch()` (`session.rs:1431-1433`) + `session.touch()` after
  send (`mesh.rs:5709, 8603, 10923`).
- **Fix:** read the clock once per dispatch/send and pass it down, or back `touch()`
  with a coarse periodically-updated atomic.
- **Impact: low-medium. Confidence: high.**

### 2.8 Grant path re-resolves the peer per accepted packet, ignoring the existing node-id cache

- **Where:** `mesh.rs:4938-4953`.
- **Now:** per accepted data packet: `addr_to_node.get` → `peers.get` → session-id
  validation → **O(peers) `iter().find` fallback** — even though
  `session.cached_node_id()` (`session.rs:165`, populated at `mesh.rs:5058-5077`)
  resolves it in one relaxed load. Also clones the session Arc to re-derive what it
  already holds.
- **Fix:** `cached_node_id()` → `peers.get(nid)`; scan only on cache miss.
- **Impact: low-medium (medium at high peer counts). Confidence: high.**

### 2.9 Subprotocol handlers re-scan all peers for `from_node` that's already a parameter

- **Where:** `mesh.rs:4481-4486` (membership), `:4508-4513` (capability),
  `:4673-4678` (reflex), `:4764-4769` (rendezvous).
- **Now:** each does `ctx.peers.iter().find(...)` to compute `from_node` — shadowing
  the `from_node: u64` parameter `process_local_packet` already received from the
  same lookup. Control-plane frequency, but membership/capability announcements
  scale with mesh size.
- **Fix:** delete the scans; use the parameter.
- **Impact: low-medium. Confidence: high (literal shadowing).**

### 2.10 NACK retransmit rebuild makes a redundant full-packet copy

- **Where:** `mesh.rs:4430-4433`.
- **Now:** `builder.build(...)` already returns owned `Bytes`;
  `Bytes::copy_from_slice(&p)` adds a second alloc + full memcpy per retransmitted
  packet — loss-path only, but fires in bursts exactly when the link is stressed.
- **Fix:** `packets.push(p)`.
- **Impact: low. Confidence: high.**

### 2.11 All ingress decrypt + dispatch runs on a single tokio task (structural ceiling)

- **Where:** `mesh.rs:3456-3489`.
- **Now:** even with batched ingress, one async task synchronously runs
  `dispatch_packet` → AEAD decrypt → subprotocol routing → (with dataforts)
  per-packet capability-set synthesis (`mesh.rs:5174-5190`; see §4.1) and per-event
  greedy-observer clones. ChaCha20-Poly1305 + dispatch caps ingress at ~1 core
  regardless of syscall improvements. Nothing blocks across `.await` (dispatch is
  fully sync — good); the loop's CPU work is the bottleneck once syscalls batch.
- **Fix (larger change):** shard decrypt+dispatch by `session_id` across N workers;
  per-session replay window is already Mutex-protected and per-stream ordering is
  arrival-order (H-8 contract), so per-session sharding preserves semantics. Needs
  an ordering review for `addr_to_node`-mutating control packets.
- **Impact: medium-high at scale. Confidence: medium (architectural).**

### 2.12 Minor per-packet overheads on the publish/send path

- `mesh.rs:8514` — `publish_to_peer` calls `session.open_stream_with(...)` on every
  publish: a DashMap **write** `entry()` lock (`session.rs:565`) even after the
  stream exists. Probe with a read `get` first.
- `transport.rs:710-715` — `PacketSender::send_batch` constructs a fresh
  `BatchedTransport` per call (3 × 64-slot Vec allocs); the router holds a reusable
  one but other callers pay ~3 mallocs per batch. Document or pool.
- `mesh.rs:10956-10961` — `register_retransmit` allocates a `Vec<Bytes>` clone +
  Arc per reliable packet sent; the Vec could come from a small pool.
- `protocol.rs:523-542` — `read_events` allocates a `Vec<Bytes>` per packet (the
  event payloads themselves are zero-copy slices — good).
- **Impact: low each. Confidence: high.**

---

## §3 Routing / nRPC / reliability (`mesh_rpc.rs`, `router.rs`, `reliability.rs`, `channel/`, metrics)

Verified clean first: `pool.rs` TLS builder cache, `cancel_registry.rs` (shared
never-firing Notify for token 0, rate-limited GC), `failure.rs` (O(1) heartbeats,
callbacks outside shard locks), `reroute.rs`/`swarm.rs` (event-driven, cold
per-message), `channel/{guard,config,name,membership}.rs` (bloom + hash-keyed;
`ChannelName` is `Arc<str>`), mesh_rpc client/server core (route cache, zero-copy
codec, response drainer, DashMap pending map). `may_execute` (~40 ns fold-gated) is
by design — but see §4.2 for what's cacheable around it.

### 3.1 `FairScheduler::dequeue` allocates a Vec and iterates the whole DashMap per packet

- **Where:** `router.rs:384`, second snapshot at `:429`; per-packet callers
  `router.rs:824/852`; fed by `mesh.rs:10986` for `scheduled` streams.
- **Now:** every `dequeue()` does `let keys: Vec<u64> = self.streams.iter().map(|e| *e.key()).collect();`
  (DashMap iteration locks every shard; fresh heap alloc), and the
  quantum-exhausted path re-collects a second full snapshot and walks all queues
  again. O(S) + 1–2 allocs per packet; idle 1 ms wakeups with non-empty registries
  pay it too. This is the per-packet path for all multi-hop forwards and scheduled
  bulk streams — and the path §2.1 wants to route *more* traffic through.
- **Fix:** maintain the active-stream list incrementally — `ArcSwap<Vec<u64>>`
  snapshot rebuilt on stream-set change (insert/cleanup bump a version), or an MPMC
  ring of non-empty-stream tokens pushed by `enqueue`.
- **Impact: high. Confidence: high.**

### 3.2 Histogram records do ~14 contended atomic RMWs per RPC

- **Where:** `mesh_rpc_metrics.rs:160-176`.
- **Now:** buckets are stored cumulatively — each observation increments every
  bucket it satisfies. A fast RPC (≤5 ms) satisfies all 11 bounds → 12 bucket RMWs
  + sum + count = 14 RMWs on the same 2 cache lines, from every concurrent caller;
  runs twice per request on a node that both calls and serves (`record_latency` via
  `CallMetricsGuard::drop` + `record_handler_duration`).
- **Fix:** non-cumulative buckets — one `fetch_add` at the partition point; compute
  cumulative sums at snapshot/scrape time. 14 RMWs → 3.
- **Impact: medium-high. Confidence: high.**

### 3.3 Flow-controlled `RpcStream` spawns a task + sends a reliable packet per delivered chunk

- **Where:** `mesh_rpc.rs:514-523` (helper `:466-493`).
- **Now:** `poll_next` auto-grants 1 credit per chunk via `spawn_grant_publish` — a
  `tokio::spawn` (~1–2 µs *est.*), a `ChannelId::new` xxh3 rehash, a Vec alloc, and
  a full reliable AEAD packet — per chunk. The server side already fixed the
  identical shape for request grants: `build_request_grant_emitter`
  (`mesh_rpc.rs:1478-1548`) calls it a "spawn-storm + AEAD-storm" and coalesces
  through one drainer. The client stream-grant side never got the fix.
- **Fix:** mirror the server — accumulate delivered-chunk credits in an `AtomicU32`,
  one per-stream drainer (or `grant(window/2)` every window/2 chunks). Halves wire
  packets for flow-controlled streams and removes the per-chunk spawn.
- **Impact: medium-high for streaming workloads. Confidence: high.**

### 3.4 `ReliableStream::on_ack` scans the entire retransmit window per ACK

- **Where:** `reliability.rs:740-750`; related `on_nack` `:664-673`.
- **Now:** `pending.retain(|u| u.seq() >= ack_seq)` visits all of `pending` even
  though acked packets are (nearly) a prefix of the seq-ordered deque. Window
  auto-sizes to `MAX_RETRANSMIT_WINDOW = 16_384` (`:314`); ACKs arrive via the 1 ms
  grant drainer, so a busy bulk stream can pay up to ~16 M element-visits/sec
  *est.* Caveat: `pending` can be slightly out of order (concurrent `send_on_stream`
  registers after awaiting the socket), so a naive pop-front isn't safe. `on_nack`
  is O(missing × pending) nested scan.
- **Fix:** pop-front while `front.seq() < ack_seq` with bounded look-ahead for
  stragglers, or `BTreeMap<seq, UnackedPacket>` + range-remove — O(acked·log n);
  fixes `on_nack` too.
- **Impact: medium. Confidence: high on cost, medium-high on benefit.**

### 3.5 Per-call reply-subscription check is a global Mutex + O(N) String scan

- **Where:** `mesh_rpc.rs:3507-3515` / `:3574`; state at `mesh.rs:1523`.
- **Now:** every `MeshNode::call` runs `ensure_reply_subscription`: one process-wide
  `parking_lot::Mutex<Vec<(u64, String)>>` (up to `MAX_REPLY_SUBSCRIPTIONS = 1024`
  entries), `iter().any(...)` with a String compare per entry — all concurrent RPC
  callers serialize on it.
- **Fix:** move the hot check into the cached per-service `RpcRoute` (a
  `DashSet<u64>` of subscribed targets, or `AtomicBool` for the one-target common
  case), or `DashSet<(u64, u64 /* xxh3(service) */)>`.
- **Impact: medium (grows with concurrency × pair count). Confidence: high.**

### 3.6 `QueueGroup::select` snapshots the whole member set per publish

- **Where:** `channel/roster.rs:84-91`; caller `dispatch_recipients` `:155-186`,
  reached from every `MeshNode::publish` (`mesh.rs:8201`).
- **Now:** per publish, per queue group: collect all members into a fresh Vec to
  pick one round-robin member; the broadcaster `DashSet` is also re-collected per
  publish. Membership changes are rare relative to publishes.
- **Fix:** `ArcSwap<Arc<[u64]>>` member snapshot per group (and optionally per
  channel), rebuilt on add/remove; `select` becomes `snapshot[cursor++ % len]`.
- **Impact: medium for queue-group-heavy pub/sub, low otherwise. Confidence: high.**

### 3.7 `NetProxy::forward` copies the full packet per forward

- **Where:** `proxy.rs:310-312`.
- **Now:** new `BytesMut` + header rewrite + `extend_from_slice` of the body per
  forwarded packet. router.rs fixed exactly this with perf #18
  (`try_into_mut` + in-place `write_at`, `router.rs:728-739`); proxy.rs never got
  it. `NetProxy` is not wired into `MeshNode` (only re-exported, `mod.rs:140`) —
  hot only for standalone-proxy deployments.
- **Fix:** port the perf #18 in-place rewrite.
- **Impact: medium where NetProxy is the relay, zero otherwise. Confidence: high cost, medium reach.**

### 3.8 `getrandom` syscall per RPC for call-id minting

- **Where:** `mesh_rpc.rs:3602-3608` (call site `:3206`).
- **Now:** `mint_random_call_id` does `getrandom::fill` — an OS entropy syscall
  (~200–400 ns *est.*, worse on Windows BCryptGenRandom) per call/streaming/duplex
  open. The requirement is only unpredictability to peers.
- **Fix:** thread-local userspace CSPRNG (ChaCha-based, e.g. `rand::rng()`)
  seeded/re-seeded from OS entropy.
- **Impact: low-medium (per-call latency shave). Confidence: high.**

### 3.9 Three heap allocations per streaming chunk for a constant header

- **Where:** `cortex/rpc.rs:2281-2288` (server path of `serve_rpc_streaming`,
  `mesh_rpc.rs:2141`); `RpcHeader = (String, Vec<u8>)` at `cortex/rpc.rs:272`.
- **Now:** every non-terminal chunk builds
  `vec![(HEADER_NRPC_STREAMING.to_string(), HEADER_NRPC_STREAMING_CONTINUE.to_vec())]`
  — String + Vec + outer Vec per chunk just to say "continue".
- **Fix:** carry continue/end as a flag bit in the response envelope, or change
  `RpcHeader` to `(Cow<'static, str>, Bytes)` so static markers are alloc-free.
- **Impact: low-medium (streaming-heavy). Confidence: high.**

### 3.10 Reply/request channel hash recomputed per response and per chunk

- **Where:** `mesh_rpc.rs:1643-1645, 589-591, 476-478, 1674-1676`; `ChannelName`
  doesn't cache its hash (`channel/name.rs:77`).
- **Now:** `publish_response_to_caller` re-runs xxh3 over the reply-channel name +
  `publish_stream_id` per response; `publish_request_chunk` /
  `spawn_grant_publish` / `spawn_cancel_publish` repeat it per chunk/grant/cancel.
  The §8b reply-channel cache (`mesh_rpc.rs:1879`) removed the per-response
  `format!` but left the rehash.
- **Fix:** cache `(ChannelName, ChannelHash, stream_id)` in the `OriginKeyedLru` /
  `RpcResponseJob` / call handles. ~10–30 ns + an Arc clone per op.
- **Impact: low. Confidence: high.**

### 3.11 Per-call `service.to_string()` + deep header clone in `call`

- **Where:** `mesh_rpc.rs:3230-3237`; also `:3172`.
- **Now:** `headers.extend(opts.request_headers.iter().cloned())` deep-clones each
  header although `opts` is owned and unused after; `service.to_string()` allocs
  although the route caches `Arc<str>`; `rpc_metrics_arc().for_service(service)`
  re-hashes the service string into the DashMap per call.
- **Fix:** `headers.append(&mut opts.request_headers)` (move); encode the service
  name from the route's cached name; stash `Arc<ServiceMetricsAtomic>` in the route
  cache.
- **Impact: low (a few hundred ns/call). Confidence: high.**

### 3.12 Double DashMap probe per stream-stats record on the forward path

- **Where:** `route.rs:711-740`; callers `router.rs:713, 753`.
- **Now:** `record_in`/`record_out`/`record_drop` each do `may_admit_stream`
  (`contains_key`) then `stream_entry` (`entry`) — two shard-lock acquisitions per
  call, two calls per forwarded packet = 4 probes/packet.
- **Fix:** single `entry()` access: on `Vacant`, check `num_streams <
  MAX_STREAM_STATS` before inserting, else return.
- **Impact: low. Confidence: high.**

---

## §4 Behavior / capability folds (`behavior/`, `behavior/fold/`)

Benches reviewed: `auth_guard.rs` (bloom fast path, <10 ns target), `placement.rs`
(≤5 µs/100-candidate budget; exercises `synthesize_capability_set` per candidate),
`origin_cache_bench.rs` (establishes `Mutex<LruCache>` as house style for hot
caches). May-28 capability fixes verified landed (`sort_by_cached_key`, tag-direct
filter fast paths, `axis_key_ref`); June-9 audit covered fold dispatch / safety /
metadata / group-subnet parse but not the query-side paths below. Fold-index lookup
cost (~40 ns) is by design and not flagged.

**Theme:** the fold stores tags as canonical `Vec<String>` while every consumer
(placement axes, predicates, scope gates, greedy admission) needs parsed `Tag`s —
so parsing + allocation repeats at query time on every hot path. §4.1, §4.3, §4.12
(and partially §4.2) collapse into one fix: a change-generation-keyed cache of the
parsed per-node `CapabilitySet` (or storing parsed tags in the fold payload).

### 4.1 `synthesize_capability_set` re-parses and re-allocates the full capability set on every call

- **Where:** `behavior/fold/capability_bridge.rs:271-295`. Hot callers:
  `behavior/placement.rs:542` (per candidate in `placement_score`), `mesh.rs:5181`
  (per inbound data packet when the greedy observer is installed), `mesh.rs:10552`
  (`best_by_score`, per candidate), `mesh.rs:7859` (per Subscribe with auth gates).
- **Now:** walks the node's fold entries rebuilding a `CapabilitySet` from scratch —
  `Tag::parse(s)` per tag string (1–3 String allocs each, `tag.rs:257-279`),
  SipHash insert into `HashSet<Tag>`, `mk.clone()/mv.clone()` per metadata pair.
  ~30 tags ≈ 3–5 µs + ~100 allocs per call *est.* A 100-candidate placement
  decision pays this 100× against a 5 µs/100-candidate plan budget. Fold contents
  change rarely relative to scoring/packet rates — this recomputes an invariant.
- **Fix:** memoize `Arc<CapabilitySet>` per `node_id`, invalidated by the fold's
  existing change generation (`Fold::change_tx` / `subscribe_changes`,
  `fold/mod.rs:238`): `Mutex<LruCache<NodeId, (u64, Arc<CapabilitySet>)>>` checked
  against `*rx.borrow()` — the origin-cache pattern. `mesh.rs:5181` already wraps
  the result in an Arc.
- **Impact: high. Confidence: high.**

### 4.2 `may_execute` runs per RPC message with no caching; caller subnet/groups re-derived per candidate

- **Where:** `capability_bridge.rs:309-375`; callers `mesh_rpc.rs:2023`
  (callee-side gate, every inbound nRPC request), `mesh_rpc.rs:2956, :3048`
  (caller-side `retain(|c| may_execute(...))` per candidate per outbound call).
- **Now:** per call: fold read lock, walk target's entries, linear string scan of
  every tag, build three Vecs of allow-list entries; with allow-lists populated,
  walk the caller's entries parsing every tag through
  `SubnetId::from_tag`/`GroupId::from_tag` (hex decode). In the retain loops the
  caller is constant yet re-derived per candidate; the lock re-acquired per
  candidate.
- **Fix:** (a) verdict cache keyed `(target, tag_hash, caller)` invalidated on fold
  change generation — for the callee gate target/tag are fixed per service, so this
  is `LruCache<caller, bool>`; (b) hoist caller subnet/group derivation out of the
  retain loops; (c) one lock acquisition for the batch retain via `with_state`.
- **Impact: medium-high (per-message). Confidence: high.**

### 4.3 Intent axis clones the candidate's entire tag set per evaluation

- **Where:** `placement.rs:1208-1221` (`evaluate_required_caps`), from
  `score_intent_axis` (`placement.rs:746-790`); same shape in
  `capability_bridge::filter_by_predicate:606`.
- **Now:** `let tags: Vec<Tag> = target_caps.tags.iter().cloned().collect();` —
  deep String clones. Strict mode: once per candidate. `AnyOfLocalCapabilities`:
  re-clones per registered intent per candidate — O(intents × candidates × tags)
  String clones per placement decision.
- **Fix:** materialize once per candidate in `placement_score` and thread through;
  better, let `EvalContext` borrow (`Vec<&Tag>` or generic over iterators).
- **Impact: medium. Confidence: high.**

### 4.4 `signed_payload` clones the entire announcement before encoding — per sign and per verify

- **Where:** `behavior/capability.rs:2259-2268`; verify path `mesh.rs:7021`
  (`from_bytes`, JSON parse) → `mesh.rs:7082` (`ann.verify()`).
- **Now:** `let mut canonical = self.clone();` deep-clones the full `CapabilitySet`
  (HashSet<Tag>, metadata, allow-lists) then `serde_json::to_vec` — on every
  inbound gossiped announcement before Ed25519 verify and on every local
  re-announce. Clone ~1–3 µs + ~100 allocs on a 30-tag set *est.*; JSON encode
  several µs more. The JSON→compact-codec swap is already tracked (May-28 fix #3,
  blocked on wire compat, documented at `capability.rs:2294-2304`) — **the clone is
  untracked and independently removable.**
- **Fix:** serialize a borrowed canonical wrapper struct that emits
  `signature: None, hop_count: 0` without cloning.
- **Impact: medium (Ed25519 ~50 µs still dominates; trims ~10–20% of the non-crypto
  remainder). Confidence: high.**

### 4.5 Per-announcement fold refresh churns the secondary index inside the write locks even when nothing changed

- **Where:** `fold/capability.rs:218-273` (`on_insert`/`on_remove`), `:303-335`
  (`derive_synthetic_index_tags`), driven by `fold/mod.rs:372-415` (`apply`,
  Replace arm).
- **Now:** the steady-state refresh (same tags, bumped generation) takes the Replace
  path: `on_remove` re-parses every tag and re-formats every synthetic tag, then
  `on_insert` does it all again; `by_tag.entry(tag.clone())` clones the String even
  when the bucket exists — all while holding both the state and index write locks,
  lengthening the writer critical section that blocks every concurrent query.
- **Fix:** (a) in the Replace arm, compare old vs incoming payload tags/region/state
  and skip index remove+insert when identical (common refresh becomes a payload
  swap); (b) clone-on-first-occurrence in `on_insert` (`get_mut` then fallback).
- **Impact: medium at design scale (announcement rate × mesh size). Confidence: high.**

### 4.6 Fold primary store and inverted indexes use default SipHash

- **Where:** `fold/state.rs:86-95` (`entries: HashMap<(u64,u64), _>`, `by_node`),
  `fold/capability.rs:198-215` (`by_tag`/`by_synthetic`/`by_region`).
- **Now:** every query hashes tag strings and every candidate-set op hashes
  `(u64,u64)` keys with SipHash; `resolve_keys_all_tags`/`resolve_candidate_keys`
  do thousands per composite query.
- **Fix:** xxh3 is already a dep and used in 10+ modules — a `BuildHasher` alias on
  these maps is a drop-in ~2–4× hash-op win. Keys derive from already-verified
  announcements; if HashDoS is a concern for `by_tag`, switch only the `(u64,u64)`
  maps/sets.
- **Impact: low-medium, broad. Confidence: high.**

### 4.7 Predicate planner re-plans on every `evaluate()`

- **Where:** `predicate.rs:1309-1331` (`eval_all_in_cost_order` /
  `eval_any_in_cost_order`); hot loops `behavior/meshdb/executor.rs:568` (per
  result row), `federated.rs:496`, `required_capability.rs:69` → intent axis per
  candidate.
- **Now:** at every And/Or node, every evaluation: collect indices into a fresh
  Vec, sort by `static_cost()` — itself O(subtree) recomputation — per node per
  row.
- **Fix:** the plan is invariant per predicate — sort children once at
  construction/decode (or memoize `static_cost`); stack array for small clause
  counts.
- **Impact: low-medium (scales with rows × clauses). Confidence: high.**

### 4.8 Constant semver RHS re-parsed on every predicate evaluation

- **Where:** `predicate.rs:1256-1278` (`SemverAtLeast/AtMost/Compatible`).
- **Now:** the stored right-hand version literal goes through `parse_semver` per
  row/candidate; the planner ranks semver leaves most expensive (cost 60) partly
  because of this.
- **Fix:** parse once at construction/wire-decode and store the `SemverTriple`
  alongside the string (or `OnceLock` per leaf).
- **Impact: low. Confidence: high.**

### 4.9 `placement_score` takes the fold lock twice per candidate; resource axis does 4 tag-set scans

- **Where:** `placement.rs:536-542`; `score_resource_axis` →
  `target_axis_value_numeric` (`placement.rs:1064-1080, 1113-1179`).
- **Now:** `with_state(|s| s.by_node.contains_key(target))` then
  `synthesize_capability_set` re-acquires the same lock; the resource axis does up
  to 4 separate full tag-set scans per candidate.
- **Fix:** fold the known-check into the synthesize call (return `Option<...>`);
  collect the 4 numeric values in one pass. Subsumed by §4.1's cache.
- **Impact: low. Confidence: high.**

### 4.10 `composite_query` clones full `CapabilityMembership` payloads per match

- **Where:** `fold/capability.rs:609-624`; result type `:187`.
- **Now:** every matched row deep-clones tags `Vec<String>`, metadata `BTreeMap`,
  allow-lists — and `limit` truncates after materialization, so over-limit matches
  are cloned then dropped. Mitigation already present: the bulk bridge paths
  (`find_nodes_matching*`) use the borrow-only `with_state_and_index`; remaining
  cloning callers are the aggregator query service (per remote query RPC — warm,
  not per-message).
- **Fix:** apply `limit` during materialization; consider
  `Arc<CapabilityMembership>` payloads in `FoldEntry` so clones become refcount
  bumps.
- **Impact: low-medium (query-API dependent). Confidence: medium.**

### 4.11 Permissive-filter candidate resolution double-materializes

- **Where:** `fold/capability.rs:517-519` (full `HashSet` of all keys) →
  `capability_bridge.rs:474-496` (copy into Vec, sort, dedup); used by
  `compute/scheduler.rs:300` (`LegacyPlacement::permissive`) per scheduling
  decision.
- **Now:** O(N) HashSet build + O(N) Vec copy + sort for a result that is "all
  nodes" — the HashSet adds nothing when no tightening predicates follow.
- **Fix:** when no constraint exists, iterate `state.entries.keys()` directly (the
  borrow path supports it) or return a Vec seed.
- **Impact: low. Confidence: high.**

### 4.12 `tags_union_for` clones every tag string per enumeration call

- **Where:** `fold/capability.rs:661-674`; caller `capability_tags_for` feeds the
  dataforts greedy admission scope gate per origin resolution.
- **Now:** `seen.insert(tag.clone())` per tag per call, re-collected into a Vec.
  The batched `capability_tags_for_all` fixed the lock churn but still clones.
- **Fix:** covered by §4.1's cached set, or return `Vec<Arc<str>>` cached per node
  generation.
- **Impact: low. Confidence: high.**

**Verified clean:** fold dispatch (`fold/dispatch.rs:212-222` — one read lock, O(1)
lookup; double kind-varint decode documented, negligible vs Ed25519), aggregation
(`fold/capability_aggregation.rs:509-524, 609-620` — `TagMatcher::compile` hoists
regex/semver/axis work out of per-entry loops), safety envelope (regex compiled at
envelope-update time, per-check atomics), load balancer (`loadbalance.rs:235-275,
722` — ArcSwap used where it should be), AuthGuard fast path (benched). Pending-by-
decision items (compact codec, OnceCell projection cache) remain tracked at
`capability.rs:2294-2304`.

---

## §5 RedEX / CortEX / state / netdb

Verified clean first: heap segment zero-copy reads with lazy freeze/`try_into_mut`
(perf #51, pointer-pinned tests), no-watcher append fast path, `append_many`
single-reserve batches, disk batch appends coalescing to ≤3 `write_all`s
(`disk.rs:1020`), fsync offloaded to a worker with separate handles
(`disk.rs:46-60, 600-632`), `partition_point` reads everywhere (perf #52),
incremental cortex fold (one fold per event under one lock acquisition; atomic
watermarks; cheap no-waiter notify), RPC codec pre-sizing + zero-copy decode
(perf #84/#100, T2.2), `Arc<T>` query states (perf #96). No busy-polling found —
all background tasks signal/interval-driven; `redex/index.rs:157+` has explicit
backoff.

### 5.1 Leader catch-up reads the entire backlog per request, then throws most of it away (O(N²) total)

- **Where:** `redex/replication_catchup.rs:238` (`handle_sync_request`);
  `read_range` at `file.rs:1082`; `materialize` at `file.rs:1536`; budget gate at
  `replication_runtime.rs:1215-1243`.
- **Now:** `file.read_range(request.since_seq, local_next)` with no byte/count
  bound ("read a generous window"), then culls to the chunk budget (≤64 MiB hard
  ceiling, `:262-300`). `read_range` materializes every event — xxh3-checksumming
  every payload (§5.8) — **while holding the file's non-yielding parking_lot
  Mutex**, the same lock `append` takes. A replica 1 GB behind: every request
  scans+hashes ~1 GB to ship 64 MiB; total work O(backlog²/chunk), all stalling
  leader appends. The bandwidth-budget admission gate runs **after** the read, so a
  back-pressured retry loop re-reads the full window and discards it.
- **Fix:** budget-aware `read_range_limited(start, max_bytes/max_events)` that stops
  materializing at the chunk budget (binary-search start, sum idx-entry
  `payload_len` before touching payloads); move budget admission before the read.
- **Impact: high (semi-hot path, but egregious; degrades the hot append path via
  lock hold). Confidence: high.**

### 5.2 Replica apply deep-copies every replicated payload, despite a comment claiming otherwise

- **Where:** `redex/replication_catchup.rs:407-411`; upstream decode
  `replication.rs:560`.
- **Now:** `Bytes::from(e.payload.clone())` — the comment says "cheap clone … so we
  don't double-copy" but `Vec<u8>::clone` is a full alloc+memcpy. Upstream
  `SyncResponse::from_bytes` already did `to_vec()` per event. Each replicated
  record is copied: wire frame → Vec (decode) → Vec clone (apply) → heap segment
  (append_batch) → disk. Two of four copies avoidable.
- **Fix:** `SyncEvent.payload: Bytes`, decode via `frame.slice(..)` (the frame is
  contiguous); `apply_sync_response` takes ownership (it's the terminal consumer).
- **Impact: medium-high (every replicated record on every replica). Confidence: high.**

### 5.3 3 `metadata()` syscalls per disk append, just to record rollback lengths

- **Where:** `redex/disk.rs:821, 867, 915` (single append); `:1049, 1072, 1108`
  (batch).
- **Now:** `append_entry_inner` stats `dat`/`idx`/`ts` on every append solely to
  capture pre-write lengths for partial-write rollback — 3 stat-class syscalls on
  top of the 3 `write_all`s, roughly doubling syscall count per disk-backed append
  (the cortex ingest path uses single `append`, not batches).
- **Fix:** all length mutations go through `DiskSegment` — track lengths in
  `AtomicU64`s per file, refreshed at open/compaction (`compact_to`'s
  generation-swap must reset them).
- **Impact: medium-high for disk-backed append throughput (likely several µs/op on
  Windows, *est.*). Confidence: high it's unmitigated, medium on fix simplicity.**

### 5.4 Leader copies each shipped payload twice

- **Where:** `redex/replication_catchup.rs:296-299` + `redex/replication.rs:493-499`.
- **Now:** chunk builder does `payload: ev.payload.to_vec()` (Bytes→Vec per event),
  then `SyncResponse::to_bytes` copies each payload again into the wire buffer. The
  segment read was zero-copy; both downstream copies are not.
- **Fix:** same as §5.2 — `SyncEvent.payload: Bytes` keeps the segment slice alive
  until `to_bytes` (one unavoidable copy into the wire frame). Saves one full-chunk
  memcpy + per-event alloc on the leader.
- **Impact: medium (catch-up and steady-state replication both pass here). Confidence: high.**

### 5.5 Blocking fsync (and disk writes) on the async runtime task, per applied chunk

- **Where:** `redex/replication_catchup.rs:432-436` via
  `replication_runtime.rs:1394`.
- **Now:** `apply_sync_response` runs inline in the channel's single `select!`-loop
  task and calls `file.sync()` — `sync_all` on up to 3 files ("millisecond-range on
  Windows NVMe" per disk.rs's own comment) — plus `append_batch`'s blocking
  `write_all`s under the file lock. Blocks a tokio worker *and* the loop that
  processes heartbeats/ticks for the channel — a slow fsync delays heartbeat
  handling and can contribute to false silence detection under load. The per-chunk
  fsync is deliberate (durable-tail-before-advertise); the placement is not.
- **Fix:** `spawn_blocking` for apply (or at least `file.sync()`), or a per-channel
  blocking worker, advancing the advertised tail on completion.
- **Impact: medium (replica latency + scheduler health). Confidence: high mechanics,
  medium severity.**

### 5.6 CortEX watchers re-run the full O(N) query on every fold change

- **Where:** `cortex/memories/watch.rs:185-196`; mirrored in
  `cortex/tasks/watch.rs:166-169`.
- **Now:** each appended event (matching or not) triggers, per active watcher: full
  HashMap scan + filter + sort (`OrderBy::IdAsc` forced for dedup) +
  `Vec<Arc<T>>` alloc + deep equality compare vs the previous result + clone on
  change. W watchers × event rate × O(state size).
- **Fix:** incremental maintenance (apply the changed seq's effect to the cached
  result) is the real answer; cheaper stopgaps: debounce/coalesce change ticks (the
  watch channel already conflates), skip re-query when the folded event can't
  affect the filter, version-counter compare instead of deep Vec equality.
- **Impact: medium-high where watchers are used, zero otherwise. Confidence: high.**

### 5.7 Typed ingest builds two buffers and hashes the payload twice per write

- **Where:** `cortex/tasks/adapter.rs:489-504` + `cortex/adapter.rs:449-453`.
- **Now:** `ingest_typed` does `postcard::to_allocvec` (alloc #1), wraps in `Bytes`,
  then `CortexAdapter::ingest` allocates a second Vec and memcpys meta+tail before
  `file.append(&buf)` copies a third time into the segment. Also
  `compute_checksum_with_meta` xxh3-hashes meta+tail, then `file.append`
  immediately xxh3-hashes the same bytes again for the entry checksum.
- **Fix:** serialize directly into a single buffer with the 24-byte `EventMeta`
  slot reserved, eliminating alloc #1 and one full-payload memcpy. The double-hash
  needs a layering decision (entry checksum could reuse the meta-checksum input) —
  lower priority.
- **Impact: medium — this is the `cortex_ingest` bench path (~125 ns baseline, so
  one alloc+memcpy is measurable). Confidence: high.**

### 5.8 `materialize` re-checksums every payload on every read

- **Where:** `redex/file.rs:1536-1553`.
- **Now:** every `read_range`/`read_one`/tail backfill computes xxh3 over the full
  payload even though the data lives in the in-memory heap segment, populated by an
  append (checksummed then) or recovery replay (verifiable once at open). A
  deliberate integrity feature with regression tests — but it makes every read
  O(payload bytes) of CPU and is the multiplier that turns §5.1 into a GB-scale
  hash workload.
- **Fix:** verify once at recovery + trust in-memory reads (or config knob /
  disk-fault-path-only). If kept, §5.1's bounded read confines the damage.
- **Impact: medium combined with §5.1; low for point reads. Confidence: high on
  cost, medium on whether the integrity stance should relax.**

### 5.9 Disk writes (6 syscalls) execute under the file-wide Mutex, stalling all readers

- **Where:** `redex/file.rs:470-530`; acknowledged for `sweep_retention`/
  `append_batch` at `file.rs:1177`.
- **Now:** `append` takes `state.lock()` then runs `disk.append_entry_at(...)`
  inside it; tail registration, `read_range`, `read_one`, `len` all contend on the
  same lock. Single-writer serialization is inherent (offset assignment); readers
  waiting out the kernel write path is not.
- **Fix (incremental restructure):** memory-staged commit with the disk write after,
  or a seqlock/arc-swap snapshot index so readers never touch the writer lock.
  Substantial; weigh against §5.3 (which cheaply shrinks the lock-hold time).
- **Impact: medium under mixed read/append load; low pure-append. Confidence: high
  mechanics (documented), medium payoff.**

### 5.10 `find_many`/`count_where` are full-table scans; `RedexIndex` is unused by the typed adapters

- **Where:** `cortex/memories/state.rs:103` (+ tasks equivalent);
  `redex/index.rs` provides the tail-driven secondary index.
- **Now:** every query iterates the whole HashMap; tag membership walks each
  record's `Vec<String>`. By design for v1 (benches measure Elements(n)).
- **Fix:** wire `RedexIndex` for `where_tag`/`status` lookups → O(matches). Only
  worth it if NetDB queries are hot in production.
- **Impact: low-medium (workload-dependent). Confidence: high shape, low hotness.**

---

## §6 Dataforts blob layer (`dataforts/`)

Verified clean first: CDC chunker (`cdc.rs` — O(1) `split_to`, no-cut scan cache
killing the O(N²) re-scan), greedy cache registry/dispatch (`greedy/cache.rs`,
`runtime.rs` — BTreeMap LRU index, O(1) `origin_counts` reverse lookup, ArcSwap
capability snapshot, single-lock discipline), `TreeNodeCache` (lru crate, `Bytes`
refcount clones — the old O(N) MRU already fixed), `fetch_chunk` returning `Bytes`
(perf #184), parallel manifest fetch/store (`buffered(16)`, perf #172), table-based
hex (perf #171), DashMap refcount table, per-adapter RS-encoder cache.

**Biggest combined win:** §6.1 + §6.2 + §6.3 interact multiplicatively on the store
path — a deduplicated CDC store currently pays hash(chunk) at the chunker,
hash(chunk) again in `store_chunk`, then a full disk read + third hash on the dedup
hit, all on the async executor. Fixing all three turns the dedup hot path from ~3
full passes over the data (on the runtime) into one pass on a worker pool.

### 6.1 All BLAKE3 hashing and Reed-Solomon encoding runs inline on the tokio runtime

- **Where:** `mesh.rs:1536, 1563, 2009, 2409, 2521` (hashing); `mesh.rs:1539, 1572`
  → `erasure.rs:534-585` (RS encode); `mesh.rs:2243` (RS reconstruct);
  `transfer.rs:517`.
- **Now:** zero `spawn_blocking`/`update_rayon` in mesh.rs (verified by grep; only
  dir.rs and fs.rs use the blocking pool). Every chunk store/fetch runs
  `blake3::hash(bytes)` on an executor thread — CDC chunks are 1–16 MiB, ~1–5 ms of
  hash each *est.*; each closed RS stripe synchronously encodes ~40 MiB data +
  16 MiB parity inside `striper.push_chunk` (tens of ms), stalling every other task
  on that worker.
- **Fix:** route hashing/encoding above a threshold (~128 KiB) through
  `spawn_blocking` or a rayon pool; use `blake3::Hasher::update_rayon` for
  multi-MiB chunks (~4× faster on 4+ cores, on top of unblocking the executor).
- **Impact: high. Confidence: high.**

### 6.2 Every chunk is hashed twice on store; the directory-store path hashes ~4× and chunks 2×

- **Where:** `mesh.rs:2404-2415` (`store_chunk` re-hash); callers `mesh.rs:2009`
  (`emit_tree_chunk`), `:1536/1548/1563/1578`, `:3005`; worst case
  `dir.rs:257-261`.
- **Now:** `store_chunk` recomputes `blake3::hash(bytes)` as a second-pass guard
  even though every internal caller hashed the same bytes the line before.
  `store_dir` per file: `chunk_payload` hashes each chunk (`blob_ref.rs:1090`),
  `blake3::hash(&bytes)` hashes the whole file again for the URI, then
  `adapter.store()` re-runs `chunk_payload` over the full payload (`mesh.rs:3087`)
  and `store_chunk` hashes each chunk a fourth time — plus a full-payload
  `Bytes::copy_from_slice` (`mesh.rs:3143`).
- **Fix:** internal `store_chunk_prehashed` for trusted in-crate callers (keep the
  verifying wrapper public); in `store_dir`, build the `BlobRef` once and reuse the
  chunking instead of re-deriving in `store()`.
- **Impact: high (directly multiplies store CPU). Confidence: high.**

### 6.3 Dedup-hit stores read and re-hash the entire existing chunk

- **Where:** `mesh.rs:2449-2471` (`store_chunk_locked`).
- **Now:** the idempotent fast path does `file.read_range(0, file.len())` +
  `blake3::hash(&existing.payload)` on **every** duplicate store. CDC dedup exists
  precisely to make duplicates common — so dedup-heavy ingest pays O(chunk_size)
  read + hash per "free" chunk instead of O(1): a 16 MiB dedup hit costs a 16 MiB
  read + ~5 ms hash *est.*, on the runtime (§6.1).
- **Fix:** trust content-addressing (compare lengths only), or a bounded
  "verified-this-session" set so each slot deep-verifies at most once, or defer to
  the GC/scrub sweep.
- **Impact: high for dedup workloads. Confidence: high.**

### 6.4 Tree fetch copies the assembled range once per tree level

- **Where:** `mesh.rs:1846-1872` (internal), `:1893-1925` (leaf), `:1961-1982`
  (erasure leaf), `:3496`.
- **Now:** each recursion level returns a fresh `Vec<u8>` and the parent
  `extend_from_slice`s — a range read through a depth-D tree memcpys the full range
  D times (depth pinned ≤ 4 → up to ~4 GiB of memcpy for a 1 GiB read), with
  unpreallocated Vec growth per level, then one more conversion to `Bytes`. Leaf
  chunks copy out of refcounted `Bytes` even when fully covered by the range.
- **Fix:** thread one pre-allocated output buffer (`with_capacity(range_len)`) down
  the recursion, or return `Vec<Bytes>` segments and assemble once (zero-copy for
  fully-covered chunks via `slice()`).
- **Impact: medium-high on large tree reads. Confidence: high.**

### 6.5 RS store path copies every CDC chunk an extra time into the striper

- **Where:** `mesh.rs:1572, 1581` (`striper.push_chunk(chunk_bytes.to_vec(), cref)`);
  `erasure.rs:449` (`in_flight: Vec<(Vec<u8>, ChunkRefV3)>`).
- **Now:** CDC emits `Bytes`; `RsStriper` demands `Vec<u8>` — a 1–16 MiB memcpy per
  chunk (~40 MiB per stripe), acknowledged in a comment as "the cost of routing CDC
  output into the RS path." `encode_sep` only needs `&[&[u8]]` for data shards;
  only shards shorter than `max_len` need an owned padded copy.
- **Fix:** store `Bytes` in `in_flight`; at `close_stripe_with_rs`, pass full-length
  shards as slices and pad only short ones into scratch buffers.
- **Impact: medium. Confidence: high.**

### 6.6 Chunk serving copies every 8 KiB wire frame out of an already-refcounted buffer

- **Where:** `transfer.rs:598-599`.
- **Now:** `for chunk in bytes.chunks(DATA_FRAME_BYTES) { send_one(..., Bytes::copy_from_slice(chunk)) }`
  — `bytes` is already `Bytes`; a 16 MiB chunk served to a peer pays ~2048 allocs +
  16 MiB memcpy.
- **Fix:** iterate offsets and use `bytes.slice(off..end)`.
- **Impact: medium on the peer-transfer hot path. Confidence: high.**

### 6.7 Per-op RedexFile reopen overhead on every chunk fetch/store/exists

- **Where:** `mesh.rs:2442-2447, 2496-2502, 3776-3783` →
  `redex/manager.rs:521-526, 850-878`.
- **Now:** every `fetch_chunk`/`store_chunk`/`chunk_exists` rebuilds a
  `RedexFileConfig` (including `self.replication.clone()`, `mesh.rs:2382`) and
  calls `open_file`, whose reopen fast path still runs
  `ensure_reopen_replication_matches`: `replication.read().clone()` + router lookup
  + `config().clone()` + structural `PartialEq` — per chunk, thousands of times per
  tree walk.
- **Fix:** small LRU of open `RedexFile` handles keyed by hash on the adapter, or a
  config-identity fast path (precomputed fingerprint / skip the replication match
  for the reserved chunk-channel prefix).
- **Impact: medium (fixed cost × per-chunk fan-out). Confidence: medium.**

### 6.8 Heat-registry eviction is an O(cap) scan under the global heat mutex

- **Where:** `gravity/counter.rs:444-456` + `:458-476`
  (`BlobHeatRegistry::entry_mut`/`evict_lru`), same pattern `:249-258`
  (`HeatRegistry`); callers `mesh.rs:764-771` (`bump_heat` — every successful
  fetch/fetch_range), `greedy/runtime.rs:602-604`.
- **Now:** `evict_lru` does `counters.iter().min_by_key(...)` — a full scan of up
  to 8192 entries, fired per new hash inserted once at cap, while holding the
  single Mutex every fetch's `bump_heat` must take. A manifest fetch touching 100
  cold hashes in steady state = 100 × 8 K scans inside the global lock.
- **Fix:** monotonic-counter BTreeMap LRU index exactly like `GreedyCacheRegistry`
  already does (pattern exists in-tree), or sampled eviction.
- **Impact: medium for fetch-heavy, high-cardinality nodes. Confidence: high
  mechanism, medium workload.**

### 6.9 `store_dir` is fully sequential and buffers whole files in memory

- **Where:** `dir.rs:229-271`.
- **Now:** each file fully read (`std::fs::read`) then `adapter.store` awaited one
  file at a time — no overlap of disk read, hashing, and store I/O, unlike
  `fetch_dir`'s semaphore-bounded concurrency (`dir.rs:486-516`). Combined with
  §6.2's 4× hashing, directory send throughput sits several times below hardware;
  large files spike memory.
- **Fix:** bounded `buffer_unordered` over entries (mirror fetch_dir's `sem`/
  `byte_sem`); optionally stream large files via `store_blob_reader`.
- **Impact: medium for directory transfer. Confidence: high.**

### 6.10 FS adapter: hash on the runtime, full payload copy per store, double `canonicalize` per read

- **Where:** `fs.rs:228` (`blake3::hash` before the `spawn_blocking`, on the
  executor), `fs.rs:237` (`bytes.to_vec()` — full chunk copy per store),
  `fs.rs:155-168` (`path_within_root`: `canonicalize(path)` + `canonicalize(root)`
  = 2+ syscalls on every fetch/exists/stream).
- **Fix:** move the hash inside the blocking closure; cache the canonicalized root
  (it never changes after construction); the `to_vec` needs a `Bytes`-taking store
  variant or acceptance as API-imposed.
- **Impact: low-medium (reference/trusted-host backend). Confidence: high.**

### 6.11 Tree walk re-hashes every node `fetch_chunk` already verified

- **Where:** `mesh.rs:1797-1809`; same double-verify in reconstruction at
  `mesh.rs:2192`.
- **Now:** on node-cache miss, `fetch_chunk` hash-verifies the node bytes
  (`mesh.rs:2521`), then `walk_tree_range` immediately recomputes
  `blake3::hash(&bytes)` as explicit defense-in-depth. Nodes are small (KBs), once
  per node per uncached walk.
- **Fix:** drop the recompute or gate behind a debug/paranoid flag.
- **Impact: low. Confidence: high.**

### 6.12 Reconstruction copies every surviving shard to an owned Vec

- **Where:** `mesh.rs:2210` (`bytes.to_vec()`), `mesh.rs:2756` (repair sweep).
- **Now:** each surviving shard (data + parity, up to k+m × 16 MiB) is memcpy'd
  from `Bytes` into `Vec` to satisfy `reconstruct_data`'s
  `&mut [Option<Vec<u8>>]` contract — including full-length shards that won't be
  mutated. Degraded-fetch fallback (semi-hot under partial chunk loss; repair sweep
  is operator-driven).
- **Fix:** only short shards need re-allocation; for full-length ones,
  `try_into_mut`/`BytesMut::from` when uniquely owned, or restructure around a
  buffer-reusing reconstruct API.
- **Impact: low (degraded stripes only). Confidence: high.**

---

## Bench coverage gaps

1. **Bus level:** no bench drives `EventBus::ingest_raw` with 1/2/4/8 producer
   threads — §1.1–§1.2 live exactly in the layer `benches/ingestion.rs` skips.
2. **Mesh end-to-end:** `benches/{mesh,net}.rs` measure isolated pieces (header
   parse, frame encode, packet build, raw encrypt, pool acquire, classification,
   routing lookups) — never `dispatch_packet`/`process_local_packet` or socket
   egress. A loopback pps bench would catch §2.1, §2.2, §2.3, §2.8, §2.11.
3. **Replication:** nothing benches catch-up; §5.1 is invisible until a replica
   falls behind in production.

---

## Recommended order of attack

Ordered by (impact × confidence) / effort, with fix-reuse batching:

1. **§6.1 + §6.2 + §6.3** — dataforts store path: `store_chunk_prehashed`,
   length-compare dedup hits, `spawn_blocking`/`update_rayon` offload. Three
   findings, one PR-sized change-set, multiplicative win.
2. **§5.1 (+§5.8)** — bounded catch-up reads + budget-gate-before-read. Removes the
   O(N²) and the leader append-lock stalls.
3. **§5.2 + §5.4** — `SyncEvent.payload: Bytes` end-to-end; removes 3 of 5
   per-record replication copies. Mechanical.
4. **§4.1 (subsumes §4.9, most of §4.3/§4.12)** — change-generation-keyed
   `Arc<CapabilitySet>` cache; the origin-cache pattern, already benched in-tree.
5. **§2.1 + §3.1 together** — default sends through the sendmmsg drain *after*
   making `dequeue` snapshot-free (otherwise §2.1 routes more traffic through
   §3.1's per-packet O(S) collect). Largest TX win; gate with a pps measurement
   like the recv-side c128 plan.
6. **§1.1 + §1.2** — striped in-flight counter + per-shard-derived stats; add the
   missing multi-producer `EventBus` bench first to pin the win.
7. **§3.2, §3.3, §3.4, §3.5** — nRPC residue: non-cumulative histograms, client
   grant coalescing (port the server fix), ordered retransmit window, route-cached
   reply-subscription check.
8. **§2.2, §2.5, §2.6** — ingress slab/pool, in-place relay forward (port perf
   #18), builder header-offset framing.
9. **§5.3, §5.5, §5.6** — cached file lengths, fsync off the select loop, watcher
   debounce.
10. Remainder (low items) opportunistically, or batched as a cleanup pass: §1.4–
    §1.8, §2.7–§2.10, §2.12, §3.6–§3.12, §4.4–§4.8, §4.10–§4.12, §5.7, §5.9–§5.10,
    §6.4–§6.12.

Items deliberately **not** re-litigated here: batched-ingress default-on (parked on
the c128 gate, `PERF_AUDIT_2026_06_09_HOT_PATH.md` §1), ack-piggyback wire change
(design-complete, §2 of same), capability compact codec (wire-compat-gated,
`capability.rs:2294-2304`), and the ~40 ns fold-index design point.
