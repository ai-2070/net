# Dataforts Blog Storage V2

## Partial-state inventory of deferred v0.3 items

### D3 — per-channel send-queue priority sort

**Goal:** `Foreground` requests always poll before `Background` in the per-channel send loop, so Foreground latency is preserved even when Background fills the queue.

**Already in place:**
- `BandwidthClass` enum lives canonically in `redex::bandwidth` and rides every `SyncRequest` wire frame (v0.3 Phase D2 commit).
- `RuntimeInputs::default_bandwidth_class` + `SyncRequest.class` give the dispatcher the per-request signal.
- D2 admission gating + D4 anti-starvation already bound Background's worst-case share of the budget, so the absence of strict priority is bounded — Background can't dominate, and it can't starve.

**Missing:** the dispatcher restructure. Today `replication_runtime::tick` and the inbound handler process messages inline as they arrive; there's no central queue to sort. D3 requires either (a) splitting the inbox into per-class `mpsc` channels with a priority-poll, or (b) inserting a `BinaryHeap` over the existing single inbox. Both are >150 LoC of dispatcher rework — Kyra's defer threshold — and risk reordering / race bugs in the v0.3 ship.

**Cost to finish:** ~1-2 commits + new tests proving Background never overtakes Foreground under load. Best done as part of a dedicated dispatcher refactor, not glued onto D2/D4.

---

### D5 — resume metrics

**Goal:** Operator dashboard sees `blob_fetch_resumed_total`, `blob_fetch_chunks_skipped_on_resume_total`, `blob_fetch_bytes_skipped_on_resume_total` so they can tell what fraction of a TB-scale fetch hit local cache vs needed cross-node pulling.

**Already in place:**
- Nothing on the counter side.
- The plan's underlying assumption (local refcount table tracks which chunks the receiver already has) holds via `BlobRefcountTable`.

**Missing:** an instrumentation site. The current `MeshBlobAdapter::fetch_chunk` reads from the local redex channel only — every `Ok` is a "local hit" by construction, every `Err` is a NotFound. There's no "I had to pull this from peer X, that took N ms, and now I'm caching it" decision point to count against.

**Why deferred:** D5 isn't shippable until the substrate has a cross-node blob fetch path that explicitly pulls + stores. That path doesn't exist in v0.3 — blob chunks cross the wire via the replication runtime (the same path D2/D3 target), but that runtime delivers chunks as side effects of the sync-response stream, not as on-demand pulls. The resume-metrics counters would attach to that future pull-and-store call site.

**Cost to finish:** trivial (3 atomic counters + 3 Prometheus exports) ONCE the cross-node fetch path is concrete. The blocker is the prerequisite path, not the metric.

---

### Persistent stripe-index journal

**Goal:** GC stripe-membership pin (Phase C6) survives process restart, so a degraded-stripe parity chunk's refcount briefly dropping to zero across a crash + restart doesn't lose the only thing keeping the stripe recoverable.

**Already in place:**
- `StripeMembershipIndex` is fully functional in memory (`adapter::net::dataforts::blob::stripe_index`).
- C6 GC pin consults the index before sweeping; tested end-to-end.
- Lazy on-read population (every `ErasureLeaf` decoded during a `fetch_range` re-registers its stripes) closes the cold-start gap *for blobs that actually get read*. Hot blobs are re-protected within one fetch of restart.
- Dedup-on-register prevents the index from bloating across repeated reads.

**Missing:** disk persistence. After a restart, a cold blob that no client reads stays unprotected against parity-sweep loss until something touches it. This is a real exposure for archival / dormant-RS-blob workloads.

**Why deferred:** requires picking a journal format (`<dir>/stripe_index.bin`), an atomic-write story (write-then-rename vs WAL), a corruption-recovery path, and a compaction strategy as stripes drop out. ~200-300 LoC + nontrivial test surface (crash injection). The operator-driven `repair_blob` remains the durable recovery for the failure mode this would prevent; for v0.3 ship the in-memory + lazy combination is the documented middle ground.

**Cost to finish:** ~2 commits — the on-disk format + replay, then the compaction sweep. Best landed alongside the next blob-store on-disk format pass so the format decisions cohere.

---

### Scheduled repair cadence

**Goal:** Plan-spec'd `RedexFileConfig::blob_repair_cadence` — a background timer that periodically walks every reachable RS blob and runs `repair_blob` on each, so degraded stripes self-heal without operator intervention.

**Already in place:**
- `MeshBlobAdapter::repair_blob(blob_ref) -> RepairReport` works (Phase C7) — given a `BlobRef`, it walks the tree, reconstructs missing data, re-stores under content-addressed hashes, and reports per-stripe outcomes.
- The CLI exposes it as `net blob repair <hash>` (Phase D6 partial), so operators can drive single-blob repair today.

**Missing:** a *registry of blob roots*. The substrate's chunk store is keyed by chunk hash; nothing tracks "the set of BlobRef::Tree roots this node holds." Without that list, the scheduled sweep has nothing to iterate over. Possible sources: walk every chunk on disk and decode bytes as a candidate TreeNode (slow; O(disk); produces false positives where non-tree chunks happen to deserialize); persist a roots index alongside the stripe-index journal; have an upstream registry inform the adapter.

**Why deferred:** the prerequisite (root registry) is substantial and design-not-locked. Operator-driven repair handles the failure mode today; the cadence is an automation convenience, not a correctness requirement.

**Cost to finish:** ~3 commits once a root-registry design lands — registry persistence, timer infrastructure (a tokio task on the adapter), and the cadence config knob + tests.

---

### D6 `throughput` CLI subcommand

**Goal:** `net blob throughput` — print operator-facing throughput observations (resume effectiveness, bytes pulled per class, hit ratios) the way `net blob metrics` prints the Prometheus body.

**Already in place:**
- `net blob metrics` ships and prints the existing counter family.
- `net blob repair`, `tree`, `verify`, `path` ship.
- `BandwidthClass` is observable on the wire and at the gate.

**Missing:** the counters `throughput` would print. D5 owns `blob_fetch_resumed_total` and family; without them, `throughput` has no signal to surface beyond what `metrics` already shows.

**Why deferred:** strict dependency on D5. Once D5 lands, `throughput` is a ~50 LoC CLI wrapper that formats the resume / per-class / per-blob counters in a human-readable rollup.

**Cost to finish:** one commit, blocked only by D5.

---

## Summary table

| Item | Pieces in place | Missing | Blocker | Effort |
|---|---|---|---|---|
| **D3** queue priority | Class on wire + in dispatcher inputs; D2/D4 bound worst case | Per-class queue + priority poll | Dispatcher refactor scope (>150 LoC, ship risk) | 1-2 commits + tests |
| **D5** resume metrics | Plan-spec'd counter names | Counters + instrumentation site | No cross-node fetch path exists yet | trivial (post-prereq) |
| **Persistent stripe-index** | In-memory index + lazy on-read repopulation | On-disk journal + replay + compaction | Format decisions tied to next on-disk pass | ~2 commits |
| **Scheduled repair** | `repair_blob` + per-blob CLI work | Root registry + background timer | No blob-root registry exists | ~3 commits |
| **`throughput` CLI** | CLI scaffolding ready | The counters it would print | D5 | 1 commit |
