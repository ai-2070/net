# Dataforts — Features

> Status: Scoping / design doc. The "Dataforts" name was originally a brainstorm wishlist for a mesh-native data plane on top of Net. After per-feature analysis, most of the wishlist already ships in Net core or falls out of existing primitives; the remainder clusters into a small genuinely-new-work list. This document is the audit so the scope of any future Dataforts work is clear and does not re-invent existing machinery.

## TL;DR

Of the 28 original wishlist items:

- **~25 ship today or fall out of existing primitives** — RedEX, CortEX, NetDB, capability announcements, proximity graph, daemon replica/standby/fork groups + Mikoshi, causal chains, AuthGuard.
- **4 are genuinely deferred** — all clustered around raw RedEX storage-layer replication and data-gravity meta-routing.

**Dataforts is not a separate product to build.** It is mostly *naming and packaging* what already exists, plus a focused storage-replication addition that is parked until a real workload requires it.

## The primitives that cover most of the wishlist

The audit below repeatedly refers back to these existing primitives. Each is shipping today.

- **RedEX** — local append-only streaming log. 20-byte index records, INLINE+heap payload hybrid, optional disk persistence, count/size/age retention. `append`, `append_batch`, `append_bincode`, `tail`, `read_range`. (`docs/REDEX_PLAN.md`)
- **CortEX** — local fold from RedEX tail into reactive in-memory state. Materialize-class but local; queryable through NetDB.
- **NetDB** — query façade (`find_unique`, `find_many`, `count_where`, snapshot encode/decode) over CortEX adapters.
- **Capability announcements** — nodes advertise hardware, models, tools, capacity. Indexed locally on every peer; routed multi-hop via the proximity graph. Used for placement, dispatch, and discovery. ~2.67M ops/sec on M1 Max.
- **Proximity graph** — local view of neighborhood + derivation from neighbors. Routes work toward nodes that can do it; routes reads toward the nearest node holding the data. Capability + proximity weighting is how the mesh "decides" where things go.
- **Daemon replica / standby / fork groups + Mikoshi** — replica groups are N interchangeable copies with deterministic identity; standby groups are one active + N-1 warm with periodic snapshots; fork groups are N independent entities with cryptographic lineage. Mikoshi handles state migration between nodes. Auto-replication of *daemon state* is fully covered.
- **Causal chains (`CausalEvent` / `CausalLink`)** — 24-byte cryptographic links between events. Provides content addressing for stream chunks, lineage tracking, and ordering. Self-authenticating; any node can verify the chain.
- **AuthGuard** — wire-speed bloom-filter ACL on every publish/subscribe. ~20ns per check. Provides segment isolation, capability-gated access, and instant fleet-wide revocation.
- **`ChannelName` + `ChannelConfig`** — hierarchical naming for channels and the files RedEX maps onto them. Per-channel policy, ACLs, retention, fanout limits.

---

## Features already shipping (or free via existing primitives)

### 1. Streaming writes — Shipping

Covered directly by RedEX. Append-only streaming log is the substrate's primary write shape. `append`, `append_batch`, `append_bincode` form the API today.

### 2. Timestamped records — Shipping (partial)

RedEX's per-file `seq` is a monotonic per-file sequence number, allocated by the appender via `AtomicU64::fetch_add`. Wall-clock timestamps are optional and live in the payload; for time-based retention, RedEX persists a `ts` sidecar. This is sufficient for time-ordered access patterns. The substrate is deliberately causal-time, not wall-clock-time, in its consistency model — wall-clock is up to the caller's payload schema.

### 3. Chunked storage retrieval — Shipping

RedEX segments are addressable byte regions per file. Each event's `(payload_offset, payload_len)` resolves to a chunk in the segment. `read_range(start, end)` retrieves a bounded scan of the hot tier. Chunks are first-class.

### 4. Queries / time-series access patterns — Shipping

CortEX folds RedEX tails into reactive in-memory state. NetDB exposes `find_unique`, `find_many`, `count_where`, and snapshot encode/decode over the CortEX adapters (currently Tasks and Memories). For time-series queries, a fold over `(timestamp, dimension)` data with a CortEX adapter exposing window queries is the pattern. No new substrate work needed.

### 5. Metadata tagging — Shipping

Two layers cover this:
- Channel-level: `ChannelConfig` carries policy and capability tags
- Event-level: `EventMeta` carries dispatch, flags, origin hash, sequence number, and checksum on every event in the bus
The combination handles per-channel and per-event metadata without needing a separate tag store.

### 6. Segment isolation / access control — Shipping

`ChannelName` + `AuthGuard` provides this. Each RedEX file maps 1:1 to a channel; channel ACLs (`publish_caps`, `subscribe_caps`) gate `append` and `tail`. Wire-speed bloom-filter check (~20ns) on every operation. Permission tokens delegate down chains with end-to-end signature verification. Revocation is instant fleet-wide via the AuthGuard's bloom-filter propagation.

### 7. Throttling bandwidth — Shipping (partial)

Backpressure is the substrate's default — nodes that can't keep up go silent, and the proximity graph notices within a heartbeat. Per-node and per-peer rate limiting is enforced by device autonomy rules. The substrate-level primitives are in place; per-application policy is layered on top.

### 8. Causal chains — Shipping

`CausalEvent` (24 bytes) and `CausalLink` are core protocol primitives. Every event a daemon produces is signed into a causal chain that any node can verify. The chain itself is self-authenticating — no database, no ledger service. Used for ordering, lineage, content addressing, and audit. Already shipping.

### 9. Auto replication — Shipping (daemon state); deferred (raw RedEX storage)

This is the row I previously over-deferred. The Net's daemon model already provides auto replication of *daemon state*:
- **Replica groups** — N interchangeable copies with deterministic identity, load-balanced across the mesh.
- **Standby groups** — one active, N-1 idle with periodic snapshots; promote on failure with replay of the gap.
- **Fork groups** — N independent entities with cryptographic lineage.
- **Mikoshi** — state migration between hardware boundaries with snapshot/replay.

CortEX folds, in-flight RPC state, and any daemon's working memory all replicate via these primitives.

The narrow piece that *is* deferred is **raw RedEX log-segment replication across nodes for storage durability**, per the explicit "no replication" constraint in `REDEX_PLAN.md` v1. That sub-feature ships when the storage-layer Dataforts work activates (see "Genuinely deferred" below).

### 10. Overflow to mesh — Shipping (daemon level); deferred (storage burst)

The daemon replica/standby groups + Mikoshi machinery handles "this node is overloaded, move work elsewhere." Storage-side overflow (RedEX log-segment burst to a peer when local disk fills) is the deferred piece, again tied to the storage-layer replication work.

### 11. Read-write API that feels local — Shipping

CortEX's reactive in-memory state + NetDB's query façade is the local API. A `Vec<Task>` or `HashMap<Uuid, Memory>` is held directly in user code, updated event-by-event from the RedEX tail. Queries are direct memory access at cache speed (`find_unique` at 8.98ns, `find_many` at hundreds of MB/sec on M1). The "feels local" property is shipping by virtue of how the local fold is structured.

### 12. Content-addressable via causal chains — Shipping (partial)

The causal chain is the addressing primitive. Each event has a `CausalLink` parent reference; the chain itself is content-addressed via cryptographic hashing. For *small payloads*, this is fully shipping. For *large blobs*, a CAS layer with manifest pointers is required — see `REDEX_MANIFEST_POINTER_DESIGN.md` and the deferred items list.

### 13. Causal chain profile — Shipping (partial)

The causal chain primitive carries `origin_hash`, `seq`, and per-event signature. Richer profile metadata (chain provenance summaries, role/source classifications, etc.) is layered on top via `EventMeta` flags and channel policy. The substrate primitive ships; richer profile schemas are an application-level concern.

### 14. Lineage tracking — Shipping (free)

Falls directly out of the causal chain. Every event's parent links form a lineage DAG that any node can traverse and verify. For *lineage queries* (e.g., "show all events that contributed to this state"), the CortEX query surface handles it via fold-time projection. No new lineage subsystem needed.

### 15. Streaming first — Shipping

Core design principle of Net (`Properties` section of the README). Tail subscriptions are first-class; the bus is unbounded streams, not request/response. Adaptive batching, sharded ring buffers, and zero-copy forwarding are all built around the streaming-first assumption.

### 16. Stream chunks as addressed — Shipping (causal-chain addressed); deferred (CAS for large blobs)

I previously over-deferred this. Each chunk in a RedEX stream has a `(ChannelName, seq)` address and a `CausalLink` for cryptographic addressing. Causal-chain addressed streaming is fully shipping. The deferred piece is specifically a content-addressable blob store for large payloads where the chunk itself is segment-sized — see `REDEX_MANIFEST_POINTER_DESIGN.md`.

### 17. Auto replication on node failure — Shipping (daemon level); deferred (storage-level)

Same as #9. Standby groups handle daemon-state failover with snapshot replay. Storage-level RedEX replication is the deferred piece.

### 18. Route reads to nearest replica — Shipping (free, proximity graph)

This is just proximity graph use. The capability index already locates nodes advertising the relevant data/capability; the proximity graph weights routing by distance + load. A read query for "data X" goes to the nearest node holding X. No new mechanism needed; it is a direct application of the existing routing primitive.

### 19. Bloom filters for fast lookups — Shipping

AuthGuard uses bloom filters for ACL revocation propagation at ~20ns per check. The same primitive can be reused for any other "is X in the (revoked, allowed, known) set" lookup. Already a building block in the substrate.

### 20. Causal consistency — Shipping

Default consistency model of the substrate. Vector clocks, Lamport timestamps, and causal-chain parent hashes give partial ordering. The mesh has no global truth; each node's view is causally consistent within its observation window. Ordering is cryptographic, not temporal.

### 21. Tombstones — Shipping

Built into RedEX from v1 — `RedexFlags::TOMBSTONE` is a flag bit in the 20-byte record. Compaction sweeps drop tombstoned records; readers see the absence. No design work needed.

### 22. Partial updates (append-only) — Shipping

Append-only IS the data model. "Updates" are new events that supersede; the fold (CortEX) decides what supersedes what. There is no in-place mutation; if you need a mutable view, you fold.

### 23. Atomic operations — Shipping (per-batch within a file)

`append_batch` is per-batch atomic: all events land in the index contiguously or none do. Cross-file or cross-segment atomicity is explicitly out of scope per `REDEX_PLAN.md` non-goals. For the current target workloads, per-file batch atomicity is sufficient. Cross-segment transaction support would only be revisited if a real workload requires it; no plan to build it speculatively.

### 24. Rate limits — Shipping

Per-node and per-peer rate limits are enforced by device autonomy rules. Each neighbor enforces its own limits independently.

### 25. Batch operations — Shipping

`append_batch` is first-class in RedEX v1. Performance: 1.72μs for 64×64B events on M1 Max (37.2M elements/sec). Batching is the recommended pattern for high-rate ingestion paths.

---

## Genuinely deferred features

The four features below are the actual scope of future "Dataforts" work, all clustered around storage-layer replication and data-gravity meta-routing.

### Raw RedEX log-segment replication across nodes (covers items 9, 10, 17 narrow cases)

`REDEX_PLAN.md` v1 explicitly defers replication. The plan: "Replication (distributed RedEX) — not planned yet. Needs real single-node usage, a clear DST story, and concrete requirements from pilots before we design it."

When this ships, it covers:
- Auto-replicating storage segments to peers for durability beyond the local node
- Overflow of storage to mesh peers when local disk fills
- Recovery from node failure at the *storage* layer (the daemon layer is already covered)

The design will likely ride on `ChannelPublisher` / `SubscriberRoster` / the causal-chain machinery already in core. Estimated effort when activated: 6-12 weeks of focused work, additive to RedEX rather than a redesign.

### Data gravity (migrate data toward frequent reads) — feature 19

The substrate routes capability-aware via the proximity graph, but it does not *dynamically migrate stored data toward read patterns*. That is a meta-layer requiring:
- Read-pattern telemetry per `(ChannelName, byte-range)`
- A migration decision policy (when to move, where to, in what increments)
- Consistency machinery for the migration window

This is premature optimization until we have real read-pattern data from production deployments. Park; revisit when the telemetry justifies it.

### Read-your-writes guarantees — feature 22

Currently the substrate is causally consistent without read-your-writes guarantees.
RYW would require:
- Session-token / version-vector machinery
- Read path that waits for `seq >= last_write_seq` before returning
- Trade-off documentation (latency vs. RYW guarantee)

### Content-addressable blob storage layer (covers items 12 and 16 large-payload cases)

For workloads where individual chunks are large, the streaming-log + INLINE+heap pattern is wrong-shaped. A separate blob store with manifest-pointer indirection from RedEX events is the design — see `REDEX_MANIFEST_POINTER_DESIGN.md`.

This is the single most concrete deferred work. Until then, RedEX's existing payload model is sufficient.

### Greedy dataforts (opportunistic LRU caching) — feature 21

A node sees streams flow past via the proximity graph + capability index. If it has spare storage capacity, it caches a copy locally. When the cache fills, evict LRU. No coordination, no replica-set membership, no orchestration. Popular data ends up cached widely; unpopular data lives only at origin. BitTorrent-flavored in spirit, but native to the substrate.

Rule: if I have the storage, I have to have the file. A node that sees a file's capability announcement and has spare storage pulls a copy. When storage fills, evict LRU to make room. No passive observation requirement; no policy negotiation. The only threshold is "do I have room."

Why this fits cleanly:

- **Capability + proximity already routes traffic past nodes.** Greedy nodes piggyback on the routing they're already participating in.
- **AuthGuard gates which nodes can cache what.** Only nodes with `subscribe_caps` for a channel can decrypt and cache its events. Encrypted relay means nodes that lack caps can't cache even if they wanted to. ACL compliance falls out for free.
- **Causal-chain verification is local.** Any node receiving cached data verifies the chain itself; no trust required of the caching node.
- **No new wire protocol needed.** Greedy nodes use the existing tail/read primitives; the only addition is the local "cache LRU on observed events" decision logic.

Trade-offs vs. the orchestrated replication work:

| Property | Greedy LRU | Orchestrated replicas |
|---|---|---|
| Coordination | None | Replica-set membership, leader/follower |
| Durability guarantee | Probabilistic (depends on cache popularity) | Strong (configurable replication factor) |
| Bandwidth | Only what flows past | Push to all replicas regardless of demand |
| Where data lands | Wherever LRU + proximity put it | Configured/policy-driven |
| Storage cost | Self-limiting (LRU evicts) | Bounded by replica factor |
| Best for | Read-heavy popular data | Durability-critical write paths |

The two are complementary, not redundant. Greedy LRU handles "make popular data fast and resilient cheaply"; orchestrated replication handles "guarantee this critical data survives N node failures." A real Dataforts deployment would likely want both eventually.

Effort estimate when activated: ~2-4 weeks. Simpler than orchestrated replication because there's no coordination protocol; could ship first as a partial answer to durability/data-gravity concerns.

---

## See also

- `REDEX_PLAN.md` — the v1 local log primitive
- `REDEX_V2_PLAN.md` — single-node v2 additions (tiering, time retention, indices, typed wrappers, ordered append)
- `REDEX_MANIFEST_POINTER_DESIGN.md` — the design for blob/manifest indirection
- `REDEX_SCHEDULER_PLAN.md` — deterministic scheduler (parked)
- `CORTEX_ADAPTER_PLAN.md` — fold installation and query surface
- `NETDB_PLAN.md` — query façade design
- `NRPC_DESIGN.md` — request/response on the bus
