# Dataforts — Features

> Status: Scoping / design doc. The "Dataforts" name was originally a brainstorm wishlist for a mesh-native data plane on top of Net. After per-feature analysis, most of the wishlist already ships in Net core or falls out of existing primitives; the remainder clusters into a small genuinely-new-work list. This document is the audit so the scope of any future Dataforts work is clear and does not re-invent existing machinery.

## TL;DR

Of the 28 original wishlist items:

- **~25 ship today or fall out of existing primitives** — RedEX, CortEX, NetDB, capability announcements, proximity graph, daemon replica/standby/fork groups + Mikoshi, causal chains, AuthGuard.
- **The remainder ships across two coordinated releases:** **The Warriors** (precursor — substrate foundations) and **Rebel Yell** (Dataforts — thin compositional layer on top of The Warriors).

**Dataforts is not a separate product to build.** It is mostly *naming and packaging* what already exists, plus targeted foundation work in The Warriors and a thin compositional layer in Rebel Yell. After The Warriors lands, Dataforts becomes **just a 4th capability category** (alongside `hardware`, `software`, `devices`) rather than a separate architectural component.

### The Warriors (precursor) — substrate foundations

Foundation work shipped together:

1. **Capability taxonomy reorganization.** The flat capability-tag namespace becomes a typed three-axis ontology:
   - **`hardware`** — what the node *can do* compute-wise (CPU cores, GPU, RAM, NIC, storage)
   - **`software`** — what the node *currently runs* (models loaded, daemons installed, tools available)
   - **`devices`** — semantic role tags (e.g. `printer`, `temperature-sensor`, `brake-controller`, `LIDAR`, `pump`, `valve`)
2. **Capability-tag discovery primitive + metadata field.** Two parallel mechanisms with distinct purposes:
   - **Tag set (set-membership, fast):** `causal:`, `heat:`, `scope:`, `fork-of:` tag shapes plus bloom-filter aggregation. The discovery layer that collapses every later phase's coordination problem; queries against tags use the existing capability index at sub-microsecond latency.
   - **Metadata field (key-value, richer):** new `CapabilitySet::metadata: BTreeMap<String, String>` carrying arbitrary application-defined key-value pairs. Reserved keys consulted by the placement filter: `metadata.intent`, `metadata.colocate-with`, `metadata.colocate-with-strict`, `metadata.priority`, `metadata.owner`. Application-defined keys propagate as opaque pairs. The Kubernetes parallel: tags = labels (set-membership, scheduler-relevant); metadata = annotations (key-value, freeform).
3. **Federated query primitives.** Composable operators over the capability index — `filter`, `match`, `traverse`, `aggregate`, `nearest`. Not a full MeshDB; just the primitives Rebel Yell composes against.
4. **Generalized 5-axis `PlacementFilter` primitive + Mikoshi integration.** Placement becomes a substrate primitive applied uniformly to data and compute — same trait scores chain caching, replica placement, and daemon migration. Mikoshi's existing daemon-migration logic gains 5-axis target selection (scope + proximity + capability-preference + colocation + compute-capacity). Replica/fork/standby groups inherit the same primitive for member placement.
5. **RedEX V2 — raw log-segment replication.** The wire protocol (`SUBPROTOCOL_REDEX`) that v1 explicitly defers. Strong durability beyond single-node. Replica placement uses the `PlacementFilter` primitive shipped above.

### Rebel Yell (Dataforts) — thin compositional layer

After The Warriors, Dataforts is just a 4th capability category. Storage capacity + hosted causal chains are advertised via the same tag namespace as compute capabilities. The remaining phases compose against the foundations:

- **Greedy-LRU dataforts** — five-axis filter (scope + proximity + capability-preference + colocation + storage); intent-tagged replication; specialized fleets emerge organically
- **Data gravity** — heat-counter annotations on capability tags; gravity emerges from greedy + heat + capability-preference automatically
- **BlobRef hook trait** — substrate carries a content-addressed *reference* through events; bytes live in the customer's existing storage layer (S3, Ceph, IPFS, on-prem); no built-in blob CAS to operate
- **Read-your-writes** — optional, post-replication, session-bounded consistency

Post-Rebel-Yell capability ontology: **four orthogonal axes** (`hardware`, `software`, `devices`, `dataforts`) all queryable via the same federated query primitives. A user can issue a single composable query like `hardware.gpu AND software.model:llama-3-70b AND dataforts.has_chain:Y AND proximity < 50ms` — that's the visible product win.

One design move collapses much of the deferred work: **causal chains advertised as capability tags**. A node holding (or willing to serve) a chain emits a `causal:origin_hash[:tip_seq]` capability tag, and every existing primitive (proximity graph, AuthGuard, capability index, hierarchical summarization) handles propagation, ACL gating, and lookup with no new wire protocol. Discovery for replication, greedy caching, data gravity, and blob references all reduces to a capability-index query. See "Discovery primitive: causal chains as capability tags" below.

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

## Discovery primitive: causal chains as capability tags

Before the per-feature deferred work, one design move collapses much of the discovery layer the deferred features would otherwise need: **causal chains advertised as capability tags**.

A node that holds (or has cached, or is willing to serve) a chain advertises a tag of the form `causal:origin_hash[:tip_seq]` — or a bloom-filter aggregate for many chains at once. The existing capability-announcement machinery handles everything else:

- **Identity is cryptographic, not naming-coupled.** The advertisement is keyed on `origin_hash` (the daemon's ed25519 public-key fingerprint), not on `ChannelName`. Renames, migrations, and channel-name reorganizations don't invalidate advertisements; identity lives in the hash. Same model as the rest of Net's identity invariants.
- **Granularity is the chain root.** 32 bytes for `origin_hash` + 8 bytes for `tip_seq` = 40 bytes per chain raw, much less under bloom-filter compression. A node holding 10,000 chains fits a full advertisement set in roughly 400 KB raw, far less compressed.
- **AuthGuard gates announcements.** Only nodes with `subscribe_caps` for the channel can decrypt and use the advertisement. ACL compliance falls out for free. Encrypted relay means nodes that lack caps can't even read advertisements not meant for them.
- **Proximity graph propagates them** at ~374 ns per announcement on M1 Max. Same hierarchical summarization that handles general capability summaries handles chain advertisements.
- **Capability index handles local lookup.** "Find nearest node holding chain X" is the same query shape as "find nearest GPU node." Routing decisions for reads use existing match-by-capability machinery.
- **No new wire protocol.** Reuses existing primitives end to end.

Update frequency is the one operational concern: do not re-announce on every event, or announcement traffic explodes. Advertise tip ranges at intervals or thresholds (e.g. every 10 s or every 1 K events). The chain self-verifies on actual read, so the advertisement is a discovery hint, not a security primitive.

What this enables, all using the same primitive:

- **Greedy dataforts:** node pulls chain, advertises `causal:X`. Other nodes route reads to it. Node evicts under storage pressure, withdraws the tag. No coordination protocol.
- **Replication discovery:** "find replicas of channel C" reduces to a capability-index query for nodes with `causal:origin_hash_of_C`. No separate replica-set membership protocol.
- **Data gravity:** track which chains your node gets queried for; pull popular ones; start advertising them. Migration decisions become local.
- **Blob CAS:** blobs identified by content hash advertised as `blob:hash:size` capability tags using the same pattern.
- **Lightweight read-your-writes:** publisher knows its tip; reader queries for nodes advertising `causal:X@>=tip_seq`.

**Implication for deferred-work effort estimates.** With discovery free, the deferred features shrink:

- Greedy LRU: ~1-2 weeks (was 2-4)
- Replicated RedEX: ~4-9 weeks (was 6-12; only the pull/repair/conflict mechanics need building)
- Data gravity: ~3-6 weeks (was 4-8; decisions become local)
- Blob CAS: roughly unchanged (~6-12 weeks; the blob format and manifest-pointer integration is the work)
- Read-your-writes: ~2-4 weeks unchanged

Updated total for a parallelized "Dataforts v0" (greedy + replicated RedEX + blob CAS): roughly **2-3 months of focused parallel work**, down from ~3-5 months. A meaningful collapse driven entirely by leaning on the existing capability-announcement primitive instead of inventing a parallel discovery layer.

---

## Query layer: federated reads via the capability index

The same primitive that enables discovery turns the capability index into a **distributed query layer** over chain metadata. The capability tag isn't only for "where is data X" lookups; it makes a small set of richer query shapes first-class without any new mechanism.

New query shapes that become possible:

- **Federated reads.** A query that needs chains A, B, C — the local node looks up the best replica for each in the capability index, dispatches reads in parallel, joins the results. The capability index serves as the query planner's source of truth for routing decisions.
- **Time-travel.** Advertise `causal:X[start_seq..end_seq]` rather than just `causal:X[:tip_seq]`, and "give me the state of X at seq=12345" routes to a node that holds that historical range — not just the current tip. Useful for replay, debugging, and incident investigation.
- **Lineage walks.** Walking back through `CausalLink` parents reduces to a capability-tag traversal: find the parent's `origin_hash`, query for nodes holding it, recurse. The full DAG history of a chain is queryable without a central lineage service.
- **Aggregate queries.** "How many active chains in region X with `model=Y`" reduces to a count over capability-index entries with the existing filter machinery. No materialized view needed.
- **Cohort and fork queries.** Forks advertise parent linkage as part of their tag; "find all chains forked from parent P" is a tag-match query against the capability index.
- **Cross-chain joins.** Relational join across multiple chains, with the capability index handling routing for each join input.

CortEX already provides the *local* query layer (folds + NetDB queries against in-memory state). This is the *mesh-level* federation layer above CortEX — the same architectural split as a distributed database layered over single-node engines, but with the routing primitive doing the planning work instead of a central coordinator.

Trade-offs to handle:

- **Tag richness vs. announcement size.** Every additional metadata bit costs announcement size and propagation cost. Aggregate richer metadata into bloom filters or hierarchical summaries; advertise full schema only on demand or via a follow-up RPC after an initial match.
- **Privacy.** Rich tags leak more metadata. ACL gating and subnet-local advertisement scope are the first lines of defense; encrypted tags for sensitive metadata are possible but add complexity. The general rule: only advertise what's already ACL-permitted to leak.
- **Staleness.** Tags propagate at heartbeat intervals; query results are eventually consistent over the metadata layer. The standard pattern is re-read on capability-miss; queries should treat capability data as a hint, not a guarantee.
- **Query language.** Today the capability index is queried via tag filters. Joins, aggregates, and time-travel may want a higher-level query API. This could live in NetDB as an extension, or as a separate `MeshDB` layer over the federated read path. Out of scope for the current Dataforts deferred work; flagged here as the natural next layer.

**Implication for the design space.** With this in hand, Dataforts is no longer just a "data plane" — it becomes a **distributed query substrate that happens to also handle storage**. That is a strictly bigger product than the audit doc previously scoped. The compute-marketplace use case still doesn't need any of it (Postgres handles its queries fine). But workloads where lineage queries, time-travel for incident investigation, and cross-site joins matter genuinely benefit. Like the rest of the deferred work, this is parked until a real workload demands it.

---

## Genuinely deferred features

The features below are the actual scope of future "Dataforts" work, all clustered around storage-layer replication and data-gravity meta-routing. Each assumes the capability-tag discovery primitive above.

### Raw RedEX log-segment replication across nodes — RedEX V2 (ships in The Warriors)

`REDEX_PLAN.md` v1 explicitly defers replication. The plan: "Replication (distributed RedEX) — not planned yet. Needs real single-node usage, a clear DST story, and concrete requirements from pilots before we design it." **The Warriors release is where this lands** as the foundation Rebel Yell composes against.

When this ships, it covers:
- Auto-replicating storage segments to peers for durability beyond the local node
- Overflow of storage to mesh peers when local disk fills
- Recovery from node failure at the *storage* layer (the daemon layer is already covered)

The design rides on `ChannelPublisher` / `SubscriberRoster` / the causal-chain machinery already in core, plus a new `SUBPROTOCOL_REDEX` for replication wire messages. Estimated effort when activated: 4-9 focused weeks (DST harness work gates the timeline). The placement strategy hook surface is built into Warriors; the intent-matching and colocation-preference logic plug in when Rebel Yell activates.

### Data gravity (emergent from greedy + heat counters) — feature 19

Earlier framing of this as a separate engineered system (read-pattern telemetry + migration decision policy + consistency machinery for the migration window) was over-engineered. The simpler reality:

**Data gravity = proximity graph + heat counters annotated on capability tags.**

Each chain's capability tag carries a heat counter — reads-per-window, propagated via the existing capability announcement machinery. Greedy dataforts (see below) within proximity see high-heat in-scope chains and pull them. More replicas in the high-heat zone → reads served locally → reads stop crossing zones → chain "gravitates" toward the zone where it's actually consumed.

**Gravity is emergent behavior of greedy + heatmap, not a separate system.** No migration engine, no consistency machinery, no decision policy. Two primitives composing into the desired property.

Effort estimate when activated: ~1-2 weeks (just adding heat counter annotations to existing capability tags + a small TTL/decay function on the counter). Not a separate engineering project.

### Read-your-writes guarantees — feature 22

Currently the substrate is causally consistent without read-your-writes guarantees.
RYW would require:
- Session-token / version-vector machinery
- Read path that waits for `seq >= last_write_seq` before returning
- Trade-off documentation (latency vs. RYW guarantee)

### BlobRef + BlobAdapter hook trait (covers items 12 and 16 large-payload cases) — ships in Rebel Yell

**Decision: do not build a substrate-owned blob CAS layer.** The substrate is streaming + coordination + metadata + lineage. Blob storage is a fundamentally different data shape (object PUT/GET, byte-range reads, immutable artifacts). Forcing blob CAS into a streaming substrate creates impedance mismatch.

**The 2 TB constraint as design boundary.** Modern server memory ranges from 256 GB (mid-tier) to 8 TB (Epyc 9684X). If a single payload exceeds memory, you're in object-storage territory, not streaming territory. Net should not transfer what cannot fit in server memory.

**The architectural separation:**

- **Streaming + coordination + metadata + lineage** → the substrate's job
- **Blob storage + bandwidth + replication of large objects** → the customer's existing storage layer (S3, R2, B2, IPFS, Ceph, NetApp, Isilon, on-prem)

Net carries a content-addressed *reference* (URI + hash + size) through events; bytes live in the customer's existing system. Verification happens at fetch time via the hash.

**The hook design:**

```rust
pub trait BlobAdapter: Send + Sync {
    fn store(&self, blob: &[u8]) -> Result<BlobRef>;
    fn fetch(&self, blob_ref: &BlobRef) -> Result<Vec<u8>>;
    fn fetch_range(&self, blob_ref: &BlobRef, range: Range<u64>) -> Result<Vec<u8>>;
    fn exists(&self, blob_ref: &BlobRef) -> bool;
}

pub struct BlobRef {
    pub uri: String,    // s3://, ceph://, file://, ipfs://, custom
    pub hash: [u8; 32], // BLAKE3 for content verification on fetch
    pub size: u64,
}
```

Customer implements `BlobAdapter` against their preferred backend. Net carries the `BlobRef` through events; never touches bytes; never owns the storage. The semi-imagery / seismic / lidar / video use cases are still served — just via hooks to existing storage rather than a substrate-owned blob pool.

Effort estimate when activated: **1-2 weeks** for the trait + ref type + verification helpers + tests + bindings. Down from 6-12 weeks for a full blob CAS implementation.

(A full blob CAS layer remains theoretically possible as a research-grade extension if a customer specifically can't use any existing blob backend. Unlikely, and explicitly not in either Warriors or Rebel Yell.)

### Greedy dataforts (five-axis filter) — feature 21, ships in Rebel Yell

A datafort pulls a chain when **all five** conditions hold (the first reads from the fast tag set; the next two read from the chain's metadata field; the last two are local-node decisions):

1. **Scope match.** The chain advertises a `scope:X` tag (set-membership, fast bloom-filter check) that matches one of the datafort's configured scopes (e.g., `scope:industrial-telemetry`, `scope:webcam-streams`, `scope:settlement`). Operators run focused fleets by configuring scope sets per node.
2. **Proximity bound.** The chain is within the datafort's configured proximity threshold (e.g., < 200 ms RTT) per the existing proximity graph. Nothing distant gets pulled.
3. **Capability-preference (intent-tagged replication).** The chain's `metadata.intent` value (e.g. `"ml-training"`, `"sensor-telemetry"`, `"billing-settlement"`) is consulted; the local node's advertised capability set (`hardware`, `software`, `devices` from The Warriors taxonomy) must include capabilities that *fulfill* that intent. Defaults: a GPU-rich node fulfills `intent: "ml-training"`; an edge node with sensor `devices` tags fulfills `intent: "sensor-telemetry"`; a stable datacenter node fulfills `intent: "billing-settlement"`.
4. **Colocation preference (causal-chain affinity).** If the chain's `metadata.colocate-with` value is an origin_hash and the local node already holds that other chain (or replicates it), the chain prefers to land on this node. Causal-chain affinity is a *soft* preference by default — it boosts placement scoring rather than gating outright; the `metadata.colocate-with-strict` variant is available for hard requirements.
5. **Storage available.** Local node decision; LRU eviction when storage fills.

All five derive from existing primitives:

- **Scope tag** — capability tag for fast set-membership filtering (existing primitive, no change).
- **Proximity threshold** — proximity graph already measures it.
- **Intent matching** — `metadata.intent` looked up in the `adapter::net::placement::intent` table that maps each intent value to its required capabilities; applications may register custom intents.
- **Colocation preference** — `metadata.colocate-with` resolved against the local capability index (which chains this node already holds).
- **Storage check + LRU** — local node decision, no coordination.

What this produces:

- **Specialized fleets emerge organically.** GPU-rich nodes cluster around `intent:ml-training` chains; edge nodes cluster around `intent:sensor-telemetry`; stable datacenter nodes cluster around `intent:billing-settlement`. Each fleet has scope coherence + intent fit + proximity locality without central coordination.
- **Causal-chain neighborhoods stay local.** A daemon transforming chain A → chain B → chain C, with `colocate-with:` annotations on B and C pointing to A, ends up running its full pipeline on one node. Cross-node hops minimized for related work.
- **Replication routes by purpose, not just past usage.** Training data gravitates toward GPU nodes regardless of historical reads. Sensor data gravitates toward edge nodes regardless of historical analytics. The substrate self-organizes around utility.
- **Bounded storage growth.** A datafort's storage usage is bounded by `scope ∩ proximity ∩ intent-fit` — never unbounded global hoarding.
- **Bandwidth efficiency.** Only pulls chains it can usefully serve.
- **ACL falls out for free.** Only nodes with `subscribe_caps` can decrypt advertisements; only matching scopes + intent fits can pull. AuthGuard gates without additional logic.

When combined with **heat counters annotated on capability tags** (see "Data gravity" above), the system also produces emergent gravity: high-heat in-scope, in-intent chains attract more in-zone replicas; reads stop crossing zones; chains gravitate to where they're consumed AND where the work the data enables actually happens.

Trade-offs vs. orchestrated replication:

| Property | Greedy (5-axis) | Orchestrated replicas |
|---|---|---|
| Coordination | None | Replica-set membership, leader/follower |
| Durability guarantee | Probabilistic (depends on coverage of all five axes) | Strong (configurable replication factor) |
| Bandwidth | Only chains matching all five filters | Push to all replicas regardless of demand |
| Where data lands | Wherever scope ∩ proximity ∩ intent ∩ colocation puts it | Configured/policy-driven |
| Storage cost | Self-limiting (LRU evicts within filter intersection) | Bounded by replica factor |
| Best for | Read-heavy purpose-aware data with locality | Durability-critical write paths |

Complementary, not redundant. Greedy + heat-counters cover ~98% of "make data fast and locally available where it'll be used" use cases automatically. Orchestrated replication covers the durability-critical 2% that need stronger guarantees.

Effort estimate when activated: ~1-2 weeks. All five filters are config knobs over the existing capability index; the LRU cache is a local data structure; no coordination protocol needed. The intent → capabilities lookup table is application-extensible.

---

## See also

- `REDEX_PLAN.md` — the v1 local log primitive
- `REDEX_V2_PLAN.md` — single-node v2 additions (tiering, time retention, indices, typed wrappers, ordered append)
- `REDEX_MANIFEST_POINTER_DESIGN.md` — the design for blob/manifest indirection
- `REDEX_SCHEDULER_PLAN.md` — deterministic scheduler (parked)
- `CORTEX_ADAPTER_PLAN.md` — fold installation and query surface
- `NETDB_PLAN.md` — query façade design
- `NRPC_DESIGN.md` — request/response on the bus
