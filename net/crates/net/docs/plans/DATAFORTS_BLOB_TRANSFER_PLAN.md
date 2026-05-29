# Datafort blob transfer — federation prerequisites

**Status:** Planning. Substrate-side prerequisite for the Hermes-to-Hermes federation work and the directory-transfer demo that lands the substrate's architectural claim with the audience that matters.
**Scope:** Close the gap between Datafort's local blob storage (which is mature) and cross-peer fetch via `causal:<hex>` advertisement (which is the documented follow-up not yet landed). Demonstrate it at realistic scale — `node_modules` between paired machines — in a way that makes the architectural property visible, not just the surface outcome.

## What the demo is actually demonstrating

This plan exists in service of a specific architectural claim, and the claim is the point. Stating it explicitly so the engineering work and the demo materials reinforce it rather than obscure it:

**The substrate handles blob movement as per-operation substrate primitives, transported through its UDP-managed wire (the single shared `UdpSocket` in `router.rs`), with substrate-level reliability and routing decisions, multiplexed across shared paths.** Each blob chunk is its own substrate operation. Each one routes through capability-aware paths. Each one's reliability is the substrate's responsibility, not handed off to the OS's TCP stack.

This matters because federated infrastructure requires the transport layer to participate in routing decisions at per-operation granularity. TCP-based transport (the alternative anyone reaching for "library that wraps sockets" would build) gives you reliable bytes between two endpoints; that's useful, but it abstracts away the granularity federation actually needs. When an agent calls a tool on one machine and routes the result to a tool on another machine, the data path can't be "establish a TCP connection, send the bytes, close it" — it has to be substrate-mediated movement where routing is part of the operation, where multiple operations multiplex through shared paths without each one paying connection-setup costs, where capability-auth participates in transport decisions.

The `node_modules` demo is what makes this claim empirically observable at the scale that matters. Tens of thousands of files. Each one transferring as its own substrate operation. The substrate's UDP-multiplexed transport handling the aggregate without falling over. This is the property TCP-based alternatives structurally can't reproduce, and the demo is what makes the property visible.

If the demo only shows "files moved fast," engineers reading it can dismiss it as "they wrote good tar code." If the demo shows the architectural property — per-file substrate operations, UDP-managed transport, multiplexed paths, capability-aware routing — engineers can't dismiss it because the property is genuinely different from what TCP-based alternatives produce.

**The demo materials must make this visible.** Not "200MB transferred in 28s" alone. "200MB across 32,000 individual substrate operations through UDP-multiplexed paths, with aggregate throughput X MB/s, per-operation latency averaging Y ms, peak memory under 400MB, resume-on-interruption working, byte-equivalent fidelity verified." That's what tells the engineering story the audience needs to see.

## What's already there

Reading `dataforts/blob/` and `router.rs` against current master:

**The UDP transport.** `router.rs` binds a single `UdpSocket` for the mesh node and routes every inbound datagram through application-layer dispatch. This is the load-bearing transport choice that makes the architectural claim above hold. The substrate isn't wrapping TCP — it's running its own transport over UDP with substrate-managed reliability, ordering, and routing.

**`BlobAdapter` trait** (`blob/adapter.rs`) — async, well-defined: `store`, `fetch`, `fetch_range`, `fetch_stream`, `store_stream`, `stat`, `list`. Every backend (mesh, fs, S3-shaped, noop) implements it. The contract is solid.

**`MeshBlobAdapter`** (`blob/mesh.rs`) — content-addressed store over Redex. Stores chunks as single-event chunk files. Manifest blobs decompose via CDC; small blobs live in one chunk. Replication, retention floor, auto-repair on fetch, blob heat tracking, overflow, pinning/refcounting, gravity migration — all built. Cross-node *replication* via `with_replication(...)` plumbs to Redex's existing replication runtime.

**`publish_with_blob`** (`blob/publish_with_blob.rs`) — store-then-publish helper that closes the consumer-reads-event-before-blob-readable race. Three durability levels (BestEffort, DurableOnLocal, DurableOnReplicas). Producer stores blob locally, publishes a BlobRef on a channel, consumer reads the event, dispatch layer routes through the adapter to fetch bytes.

**`causal:<hex>` advertisement convention** — exists as a capability-tag convention. Chunk channels open with this tag when `with_replication` is configured, so peers can in principle observe individual chunk advertisements. The convention is in place; the consumer side isn't wired.

**nRPC over the UDP transport.** The RPC layer rides on the same UDP socket as everything else, with substrate-managed reliability per call. This is what makes per-file fetch viable at scale — each `blob.fetch_chunk` call is an nRPC operation multiplexed through the shared UDP transport, not a new TCP connection per file.

## What's actually missing

Confirmed by the comment block at `blob/mesh.rs:32–43` and the `causal:` references throughout: cross-node replication is plumbed through Redex, but **e2e mesh integration where a peer fetches a blob via `causal:<hex>` advertisement lands in a follow-up.** Specifically:

1. **No causal-advertisement subscriber side.** Peer A advertises `causal:<hex>` when it stores a chunk; peer B has no path that consumes that advertisement to *go fetch* the chunk from A on demand. Replication pushes work today; pull-on-demand doesn't.

2. **`stat::replicas_observed` returns 0.** The accounting that would let a fetcher know how many peers hold a blob isn't wired, so even a hand-rolled fetcher would have no peer-discovery answer beyond "ask everyone."

3. **No fetch-from-peer RPC.** The substrate has nRPC over the UDP transport; blob chunks are content-addressed bytes; the missing piece is a service like `blob.fetch_chunk(hash) → bytes` that peers advertising `causal:<hash>` automatically serve, and that other peers' `MeshBlobAdapter::fetch` consults when a local lookup misses.

4. **No fallback/recovery path in `MeshBlobAdapter::fetch`.** Today it reads local Redex. The federation version needs: read local → on miss, find a peer advertising `causal:<hash>` → fetch via the chunk RPC → store locally → return.

5. **No replicas_observed update from fetch traffic.** When a peer successfully fetches a chunk from another, the local stat should update so subsequent decisions (do I need to re-replicate? is this chunk well-covered?) have real numbers.

## Plan

Three pieces of substrate work, then two test phases, then the demo materials that make the architectural property visible. Substrate work is small because the foundation is solid.

### S-1: `blob.fetch_chunk` nRPC service

**Where:** New module `net/crates/net/src/adapter/net/dataforts/blob/fetch_rpc.rs` (or fold into `dispatch.rs`).

**What:** A typed nRPC service that any node running a `MeshBlobAdapter` auto-registers. Request is the 32-byte chunk hash. Response is the bytes if local, `NotFound` if not. Capability-tag gated by `causal:<hash>` — only peers advertising that they have the chunk serve the request (avoids fan-out to peers that definitely don't).

```rust
// Request
pub struct FetchChunkRequest {
    pub hash: [u8; 32],
    pub range: Option<Range<u64>>,  // optional range fetch
}

// Response carried over the typed RPC return
pub enum FetchChunkResponse {
    Bytes(Bytes),
    NotFound,
}
```

Each call rides on the substrate's UDP transport, multiplexed with every other in-flight operation through the shared socket. No per-call connection setup, no per-call TLS handshake — the substrate's session-level auth (channel-auth, capability-auth) gates the call, and the UDP wire carries the bytes. This is the property that makes the per-file approach viable at `node_modules` scale; if each call cost a TCP-style handshake, 32,000 files would be 32,000 connection setups, which is precisely the failure mode of naive socket-based alternatives.

Streaming variant for large chunks uses `call_service_streaming` so chunks above some threshold (probably 256KB) stream rather than buffering whole-chunk in memory on either side. Streaming also rides on the UDP transport with substrate-managed reliability per stream chunk.

**Size:** ~200 LoC plus tests. The handler reads from local `MeshBlobAdapter::fetch` and returns; the substrate's capability-routing + auth + UDP multiplexing handle the rest.

### S-2: Wire `MeshBlobAdapter::fetch` to consult peers on local miss

**Where:** `dataforts/blob/mesh.rs` `fetch()` and `fetch_stream()`.

**What:** Current flow is `local lookup → return or NotFound`. New flow is `local lookup → on miss, call_service_streaming("blob.fetch_chunk", hash) against peers advertising causal:<hash> → on hit, store locally via existing store() → return bytes → on miss across all peers, return NotFound`.

The capability-tag filter does the peer selection (substrate routes to peers with `causal:<hash>`); latency-aware routing picks the closest healthy one; failover handles flaky peers. No new routing logic — uses what's already there for nRPC over the UDP transport.

**Critical:** auto-store on successful peer fetch (line "store locally via existing `store()`") means the next `fetch` for the same chunk is local. This is implicit caching as a property of how the adapter works, not a separate cache layer. It also means that during a directory transfer, files fetched once become local and subsequent fetches (if any) hit the cache — but more importantly, the same content-addressed primitive that does this also gives the substrate automatic dedup across the directory: if two files share content, the second one's chunks are already local after the first one transferred.

**Size:** ~150 LoC plus tests. Mostly wrapping the existing fetch with the fallback path.

### S-3: `replicas_observed` accounting

**Where:** `dataforts/blob/mesh.rs` stat + the chunk-channel announcement path.

**What:** When a chunk is stored locally, advertise `causal:<hash>` (already happens when replication is configured). When a peer's chunk is fetched, increment the local view of "I've now seen this chunk on N peers." The capability fold already aggregates these advertisements; the missing piece is exposing the count in `stat()`.

Probably one in-memory counter per chunk hash, updated from the fold's `causal:<hash>` aggregation. Reads of `stat::replicas_observed` consult it.

**Size:** ~80 LoC plus tests.

## Test phase 1: Simple blob transfer

**Setup:** Two mesh nodes, paired, both running `MeshBlobAdapter`. Node A stores a 1MB blob. Node B requests it via `MeshBlobAdapter::fetch(blob_ref)`.

**Assertions:**
- Node B's local fetch initially misses (chunk not in B's Redex).
- Substrate finds Node A advertising `causal:<hash>` for the chunk.
- Fetch routes to A through the substrate's UDP transport, returns bytes.
- Node B stores the chunk locally as a side effect.
- Second fetch from B is local (no peer call).
- `stat::replicas_observed` on B reports 2 (A and B both have the chunk).

**Pass criteria:** First fetch < 50ms on localhost-paired nodes. Second fetch < 5ms (purely local). Bytes returned match input byte-for-byte. No phantom calls to peers that don't advertise the chunk.

**Size:** ~200 LoC test harness. Integration test under `tests/cross_peer_blob/`.

## Test phase 2: `node_modules` and Cargo target directories — the architectural demo

**Why these two specifically:** they're the realistic stress cases that reveal whether the per-operation UDP-multiplexed transport actually delivers what the architectural claim requires. Different failure profiles:

- `node_modules` — tens of thousands of small files, deep nesting, symlinks (workspace links, binary shims), high duplication across workspaces. This is the case that proves the per-file substrate-operation pattern works at scale. TCP-based alternatives die here because connection-setup overhead dominates; UDP-multiplexed substrate operations don't pay that cost.
- Cargo `target/` directory — fewer files but larger ones (compiled artifacts, debug info). Tests large-blob streaming over the same UDP transport, memory behavior on multi-hundred-MB single files, intermediate fetch interruption.

Both are familiar enough to developers that the demo is immediately legible. More importantly, both are at scales where the architectural property matters — `node_modules` proves the small-operation case (where TCP-based alternatives can't reach the same throughput), Cargo `target` proves the large-blob streaming case (where the substrate's transport has to handle multi-GB transfers cleanly).

**Wrapper needed:** the BlobAdapter handles individual blobs; a directory has structure. A thin layer that:

1. Walks the source directory, builds a manifest of (relative_path, blob_ref, mode, symlink_target).
2. Stores the manifest itself as a blob (it's just bytes).
3. Returns one root blob_ref representing the whole directory.

Receiver fetches the root blob_ref → reads the manifest → fetches each leaf blob_ref → reconstructs the directory tree with correct paths, modes, symlinks. **The leaf fetches happen as individual substrate operations multiplexed through the substrate's shared UDP transport** — that's the architectural property the demo is making visible. Concurrency is bounded but high (probably 64-256 in-flight fetches at any moment, configurable), since UDP multiplexing handles aggregate operation count cleanly where TCP would have stalled at much lower concurrency.

This wrapper lives outside `dataforts/blob/` proper — probably `dataforts/dir/` or in the SDK layer — because it's a higher-level concept than `BlobAdapter` deals with. The blob layer stays focused on opaque bytes; the directory layer handles tree reconstruction with paths, modes, symlinks; the substrate's UDP transport carries every individual operation.

**Setup:** Two paired nodes. Source machine has a `node_modules` from a real project (1000+ files, 200MB+ total — ideally tested at 30,000+ files / 500MB+ scale for the demo). Issue a directory-transfer request to the receiver. Measure.

**Assertions:**
- Transfer completes without error.
- Receiver's reconstructed directory matches source byte-for-byte for every file.
- Symlinks point to the same relative targets they did on source.
- File modes preserved (executable bits, etc.).
- Total transfer time scales linearly with size, not quadratically with file count. **This is the architectural property test** — if scaling is sublinear with respect to file count (which it should be, because UDP multiplexing means many small files cost roughly the same as few large ones at equal byte volume), the substrate is doing what the claim says it does. If it's quadratic, something's broken.
- Memory peak on both sides stays under 500MB regardless of total transfer size (streaming, not whole-tree-in-memory).
- Network blip mid-transfer (kill connection, restore) resumes from where it stopped, doesn't restart.
- Per-operation latency stays bounded as concurrency increases (UDP multiplexing absorbing the load).

**Pass criteria for the architectural demo:**
- A 200MB `node_modules` (~30,000 files) transfers between localhost-paired nodes in under 30s.
- A 2GB Cargo `target/` transfers in under 5 minutes.
- Memory peaks under 500MB on both sides.
- Resume on interruption works.
- **Aggregate throughput at high file count matches throughput at low file count when total bytes are equal.** This is the load-bearing claim — if a 200MB transfer across 30,000 files achieves throughput within 80% of a 200MB transfer across 200 files, the per-operation overhead is genuinely amortized through the substrate's transport, which is the architectural property TCP-based alternatives can't reproduce.

These are the numbers that go in the demo materials and the PR description. Not just "fast file transfer" — the specific architectural claim, demonstrated at the scale that proves it: per-operation substrate primitives, UDP-multiplexed transport, throughput-invariant under file count at equal byte volume, capability-aware routing, content-addressed dedup.

**Size:** ~400 LoC for the directory wrapper, ~300 LoC for the test harness, plus the benchmarking instrumentation that produces the specific numbers the demo materials need.

## What the demo materials must say

The framing matters as much as the numbers. The demo materials and the surrounding documentation should make these claims explicit:

1. **Per-operation substrate primitives.** Each file in the directory transfer is its own substrate operation. Not "chunked through a stream" — addressable, routable, individually fetched through nRPC over the substrate's transport. This is what makes the substrate genuinely federated rather than "library that wraps a connection."

2. **UDP-multiplexed transport with substrate-managed reliability.** The substrate's single shared UDP socket handles every operation through application-layer dispatch. Reliability, ordering, retransmission are the substrate's responsibility — tuned to substrate semantics, not constrained to TCP's stream model. This is what makes per-operation transport viable at high operation count.

3. **Capability-aware routing at per-operation granularity.** Each fetch routes to peers advertising `causal:<hash>` for that specific chunk. The routing decision is part of the operation, not abstracted away by the transport. Federation requires this granularity; TCP-style transport doesn't provide it.

4. **Content-addressed dedup as substrate property.** Files sharing content are fetched once; the auto-store-on-fetch behavior plus content addressing means deduplication is automatic across the directory and across subsequent transfers. Not a feature of the directory wrapper — an emergent property of the substrate's primitives.

5. **Concrete scaling claim.** Throughput at high file count matches throughput at low file count when byte volume is equal. This is what makes the architectural property empirically observable rather than just asserted.

The PR description, the architecture docs, and any external write-up that emerges from this work should anchor on these claims. The numbers from the test phase support them. The substrate code in `router.rs`, `dataforts/blob/`, and the new `fetch_rpc.rs` is the primary source any sophisticated engineer reading the demo can verify the claims against.

## If both tests pass

Then the substrate has demonstrably-working cross-machine blob transfer at realistic scale, with the architectural property (per-operation UDP-multiplexed substrate primitives) empirically observable. Engineers at Google's agentic Gemini organization (and equivalent infrastructure teams elsewhere) reading the demo see the property they would need for their own internal federation work, demonstrated at scale, with the code available for them to verify against.

At that point the planning shifts to the Hermes-to-Hermes layer: how agent delegation uses these primitives, what the delegation channel looks like, how agent-to-agent identity is established on top of pairing. Separate plan, separate scope, but with the substrate certainty that the bytes and the architecture both hold under realistic load.

## Order

1. **S-1 (`blob.fetch_chunk` RPC)** first — unblocks everything else. Rides on the existing UDP transport, so no new transport work.
2. **S-2 (`fetch` fallback path)** depends on S-1.
3. **S-3 (replicas_observed)** can land in parallel with S-2; it's independent accounting work.
4. **Test phase 1** (simple blob transfer) after S-1 + S-2.
5. **Directory wrapper + Test phase 2** after phase 1 passes.
6. **Demo materials** — benchmarks, comparison numbers, architectural-claim documentation — produced from the test phase 2 results, written for the audience that reads carefully enough to see the property rather than just the speed.

Total substrate work: ~430 LoC plus tests. Total test work: ~700 LoC plus the directory wrapper at ~400 LoC. Demo materials: a few days of writing once the numbers are real. Realistic estimate: 3 weeks of focused work, with the test phases providing the hard answer about whether the design holds. If something breaks at test phase 2 (especially the throughput-invariance claim), the architectural framing changes — better to learn at test-phase-2 than at PR-review or demo time.

## What this plan does NOT include

- **No durability promises beyond what `publish_with_blob` already provides.** BestEffort / DurableOnLocal / DurableOnReplicas remain the three modes. Federation doesn't add new durability semantics.

- **No GC or pinning policy changes for the federated case.** Whatever pinning the local `MeshBlobAdapter` does, the fetched-from-peer chunk inherits.

- **No agent-layer concerns.** This is bytes between peers, demonstrating substrate architecture. Whether Hermes uses it for delegation, for file transfers, or for something else is downstream and lives in a separate plan.

- **No tar/zip alternatives or optimizations on top of the per-file pattern.** The point of the demo is the per-file pattern itself. Bundling files into archives before transfer would undermine the architectural claim by abstracting away the per-operation property. The demo specifically demonstrates the property TCP-based alternatives can't reproduce, which requires keeping the per-operation granularity visible.

## Open questions for after the test phases

These don't block the plan but are worth knowing exist:

- **How does the directory wrapper handle very large single files?** A 10GB ML model file inside `node_modules` would stress the streaming layer in ways a 200MB `node_modules` doesn't. Worth testing once basic cases pass.

- **What's the optimal concurrency for parallel chunk fetches?** Too few = leaves throughput on the table; too many = memory pressure and (less critically than with TCP) some marginal saturation. Default should be reasonable; configurable for power users. The UDP transport handles much higher concurrency than TCP-based alternatives, but it's not infinite.

- **Does the `causal:<hash>` advertisement scale to millions of chunks?** The capability fold handles a lot, but a `target/` directory might produce 50k+ chunks. Test phase 2 will give the first real datapoint.

- **What happens when a peer with a chunk goes offline mid-transfer?** Substrate failover should pick another peer if multiple advertise the chunk; if only one peer had it, the fetch fails. Test resilience explicitly.

These are real questions but they're empirical — the answer comes from running the tests, not from designing more upfront.
