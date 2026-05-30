# Datafort blob replication — automatic role assignment (wire the scale path)

**Status:** Planning. Follow-up to the federation S-1/S-2/S-3 work and the replication-mode directory transfer (`dataforts/dir::become_dir_leader` / `fetch_dir_replicated`).
**Goal:** Make the proven-at-scale replication path work **without manually driving coordinator roles**, so a `node_modules`-scale directory transfers A→B with no per-chunk advertisement and no caller-visible role dance.

## Where we are

The replication-mode transfer is **built and proven** (committed): a 25-file tree moves A→B purely over RedEX replication, past the per-chunk `causal:<hex>` advertisement ceiling, byte-for-byte. See `tests/cross_peer_blob.rs::replicated_directory_transfer_end_to_end`.

The one wart: replication has **no auto-election** (a freshly-opened channel's coordinator sits in `Idle` forever; `replication_step.rs::tick()` does nothing for `Idle`, and the only election fires from the `Candidate` path which only a `Replica` can enter). So today the caller must drive roles explicitly:

- source: `MeshBlobAdapter::become_chunk_leader(hash)` — `Idle→Replica→Candidate→Leader`
- receiver: `MeshBlobAdapter::become_chunk_replica(hash)` — `Idle→Replica`

`become_dir_leader` + `fetch_dir_replicated` wrap this, but the caller still has to call them in the right order. We want it transparent: `store` makes a node a source, `fetch`/`prefetch` makes a node a receiver.

## The key insight: role-by-intent, not election

The general RedEX "Phase F" (placement-driven auto-election) is genuinely hard and was deferred for a reason. `replication_election.rs::elect` sets **self-RTT = 0** (`replication_election.rs:123`), so at a symmetric cold start *every* node elects itself leader → dual-leader, resolved only by after-the-fact convergence (higher tail / lower NodeId concedes). Correct eventually, but racy, and for a fresh empty replica it can briefly elect an empty node leader.

**We don't need the general election for blobs.** Blob chunks are content-addressed and single-writer-authoritative: the node that *stored* the bytes IS the source of truth; a node that *wants* the bytes is a follower. So the role is known by **intent**, not by election:

- `store(chunk)` on a replication-configured adapter → this node is the **Leader** for that chunk channel.
- `prefetch(chunk)` / a cross-peer `fetch` miss → this node is a **Replica** for that chunk channel.

This sidesteps the election consensus problem entirely: no RTT race, no dual-leader, no convergence dependency. It's deterministic and correct for the content-addressed model. (If two nodes independently `store` the same content, both become Leader for that channel — the existing dual-leader convergence handles that rare case, and it's harmless because the bytes are identical.)

## Plan

### A-1: Auto-leader on store (source side)

**Where:** `MeshBlobAdapter::store_chunk_locked` (`dataforts/blob/mesh.rs`), after a successful local append.

**What:** when the adapter is `with_replication(...)`, after a chunk lands locally, drive its channel to Leader — i.e. call the existing `become_chunk_leader(hash)` logic. Gate strictly on `self.replication.is_some()`; a non-replicated adapter has no coordinator and must stay untouched (today's behavior).

**Idempotency / cost:** `become_chunk_leader` already no-ops if already Leader. The 3-step `transition_to` dance is cheap (in-memory state + a `announce_chain` side-effect) but runs per first-store-of-a-chunk. For a directory store of N chunks that's N drives — acceptable (the e2e test does exactly this in ~ms). If it shows up hot, batch the announce.

**Risk:** `store` becoming a leadership side-effect. Mitigation: gated on replication config (off by default), and Leader is the correct role for a writer. Keep `become_chunk_leader` callable standalone for explicit control.

### A-2: Auto-replica on prefetch / cross-peer fetch (receiver side)

**Where:** `MeshBlobAdapter::prefetch` (`dataforts/blob/mesh.rs`) and the S-2 fetch fallback (`fetch_chunk_from_peers`).

**What:** when replication is configured, after opening a chunk channel that this node doesn't hold, drive it to Replica (`become_chunk_replica(hash)`) so the replication runtime starts pulling. `prefetch` already opens the channel; add the role drive. For the S-2 RPC fallback, replication is an *alternative* pull — keep RPC as-is when replication isn't configured; when it is, prefer (or race) the replication pull.

**Open decision:** should a replication-configured adapter's `fetch` miss pull via replication *instead of* the `blob.fetch_chunk` RPC, or *in addition*? Recommendation: if replication is configured, use replication (reliable, scalable); fall back to RPC only when no replication. Keep them mutually exclusive by config to avoid double-pull.

### A-3: Collapse the dir helpers

**Where:** `dataforts/dir.rs`.

**What:** once A-1/A-2 land, `store_dir` on a replicated adapter auto-leaders every chunk and `fetch_dir` auto-replicas+pulls every leaf — so `become_dir_leader` and `fetch_dir_replicated` collapse into plain `store_dir` / `fetch_dir`. Keep the explicit functions as thin wrappers (or `#[deprecated]`) for one release so callers migrate. `fetch_dir` gains a "wait until local" poll for the replication path (today's RPC path returns synchronously; the replication path is async-arrival).

### A-4 (optional, deferred): general Phase F auto-election

Not needed for the blob demo. If non-blob replicated channels ever need cold-start auto-election, that's a separate, harder effort: transition `Idle→Replica` at spawn for a non-empty replica set, and make the symmetric-startup race converge to the data-holder safely (the `elect` self-RTT=0 bias + dual-leader convergence needs a careful review first). Explicitly out of scope here.

## Test plan

- **Unit:** `store` on a replicated adapter drives the chunk channel to Leader (assert `coordinator.role()`); `prefetch` drives to Replica. Non-replicated adapter: no coordinator touched.
- **E2E (rewrite the existing proof):** `replicated_directory_transfer_end_to_end` should pass with **plain `store_dir` + `fetch_dir`** (no `become_*` calls) once A-1/A-2/A-3 land — that's the acceptance test.
- **Scale:** the 100-file directory that fails today on the advertisement path must pass on the replication path. Add throughput/memory capture.
- **Regression:** the existing replication e2e (`redex_replication_e2e.rs`) and all blob tests stay green; the non-replicated `store_dir`/`fetch_dir` path (per-chunk causal + RPC) is unchanged.

## Order

1. **A-1** (auto-leader on store) — smallest, unblocks the source side.
2. **A-2** (auto-replica on prefetch/fetch) — receiver side; decide the replication-vs-RPC config switch.
3. **A-3** (collapse dir helpers + `fetch_dir` wait-for-local) — make the public surface clean.
4. Re-point the e2e + scale tests to the transparent path; capture numbers.
5. **A-4** (general Phase F election) — deferred, separate plan if ever needed.

## What this does NOT include

- **No change to the general RedEX election / placement machinery.** Role-by-intent is blob-layer-only; core replication consensus is untouched.
- **No new durability semantics.** Replication's existing windowed/retransmitting sync is the transport; we only assign roles.
- **No removal of the S-1/S-2/S-3 RPC path.** It stays as the small-chunk / non-replicated / discovery-driven path.

## Risks & open questions

- **`store` side-effect surprise.** Auto-leadership on store changes what `store` does for replicated adapters. Gated on config; documented loudly.
- **Two writers of the same content** both become Leader → dual-leader convergence. Rare, harmless (identical bytes), but verify it converges rather than thrashes.
- **Replication-vs-RPC switch** (A-2): need one clear rule (config-driven) so a fetch miss doesn't pull twice.
- **`fetch_dir` async-arrival.** The replication path delivers asynchronously; `fetch_dir` needs a bounded wait-until-local. Pick a sensible default timeout + make it configurable.
- **Pinned replica set.** Today the demo uses `PlacementStrategy::Pinned([a,b])`. For >2 nodes / general federation, the replica set / source identity needs to come from somewhere (the manifest names the source, or placement). Out of scope for the paired-node demo; note it for federation.
