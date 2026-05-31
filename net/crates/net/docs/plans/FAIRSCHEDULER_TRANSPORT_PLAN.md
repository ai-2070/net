# Datafort blob transfer — router-stream plan

**Status:** Replaces the previous plan. Built around the substrate's actual primitives rather than an imagined layering.
**Scope:** Add on-demand cross-peer blob fetch by using the router's existing stream and scheduler primitives directly, with discovery through the capability fold's existing `causal:<hex>` advertisement convention. Demonstrate at `node_modules` scale to make the substrate's architectural properties empirically visible.

## The architectural claim, stated precisely

The substrate's `router.rs` already provides every primitive needed for bulk byte movement between authenticated peers:

- **Streams as first-class primitives.** Every packet carries `stream_id: u64` in the header. Multiple streams multiplex through one UDP socket.
- **Fair scheduling per stream.** The `FairScheduler` does quantum-based round-robin across active streams with configurable per-stream weights (`set_stream_weight`) and per-stream queue depth (`max_queue_depth`).
- **Per-packet priority.** `priority: u8` and `PacketFlags::PRIORITY` let control messages and interactive operations bypass bulk traffic. `priority_bypass` config in the router enables fast-path delivery.
- **Substrate-level reliability semantics, per-packet.** `PacketFlags::RELIABLE` and `PacketFlags::NACK` mean reliability is a per-packet decision. Bulk transfer chunks ride reliable; control messages can be reliable or not as appropriate.
- **Stream lifecycle.** `PacketFlags::FIN` closes streams. Idle streams get cleaned up after `idle_timeout_ns`. No explicit open handshake needed — traffic on a new `stream_id` opens the stream implicitly.
- **Fragmentation.** Built into the wire format (`frag_flags`, `fragment_id`, `fragment_offset`). Chunks larger than `MAX_PAYLOAD_SIZE` (8108 bytes) get fragmented at the substrate level.
- **Encryption.** ChaCha20-Poly1305 per-packet, keyed off the session. Already handles confidentiality and integrity at the wire level.
- **Session and authentication.** `session_id` in the header binds packets to authenticated sessions. The substrate's existing channel-auth determines whether a peer can publish/subscribe on a given channel.

The transfer demo's architectural claim is that all of this composes correctly for high-throughput, many-stream, fairness-preserved bulk byte movement. The new code is the thin convention layer that says "blob transfer uses streams this way" — not new transport, not new scheduling, not new auth.

## What needs to be built

Five small pieces, in order.

### T-1: Subprotocol ID and stream-allocation convention

**Where:** New constant in the protocol module or in a new `dataforts/blob/transfer/` directory.

**What:** Reserve a `subprotocol_id` for blob transfer (call it `SUBPROTOCOL_BLOB_TRANSFER`). Define the stream-allocation convention: streams used for transfer have IDs in a reserved range (say, the high bit set, or a specific prefix), so they're distinguishable from RPC streams or replication streams. The convention prevents collisions across subsystems that all allocate stream IDs.

**Size:** ~50 LoC including documentation of the convention.

### T-2: Discovery-to-stream bridge

**Where:** `dataforts/blob/mesh.rs` or a new `transfer.rs` next to it.

**What:** When `MeshBlobAdapter::fetch` misses locally, consult the capability fold for peers advertising `causal:<hash>` for the requested chunk. Pick one (latency-aware via existing routing infrastructure). Allocate a transfer stream ID. Send a small control packet to the chosen peer on that stream with `SUBPROTOCOL_BLOB_TRANSFER`, carrying the content hash being requested.

The control packet is tiny — content hash (32 bytes), some framing — fits well under the 8108-byte payload max. Sent with `PacketFlags::RELIABLE | PacketFlags::PRIORITY` so it gets through quickly even under load.

**Size:** ~150 LoC. The peer selection uses existing capability fold queries; the stream allocation uses existing router primitives; the new code is the bridging logic that connects discovery to stream initiation.

### T-3: Transfer handler on the serving side

**Where:** Same module as T-2.

**What:** A handler registered for `SUBPROTOCOL_BLOB_TRANSFER` that receives the control packet on a new stream, validates authorization (the request arrived on an authenticated session; the requester is subscribed to a channel that authorizes them to read this content; existing channel-auth gates this), looks up the chunk in local storage, and sends it back on the same stream as fragmented payload packets terminated by a FIN.

The bytes get sent through the router's normal send path with the allocated stream_id, which means they automatically participate in the fair scheduler's per-stream allocation. Stream weight can be set high or low depending on whether the transfer should be aggressive or background.

**Size:** ~200 LoC including the handler registration, auth check (uses existing primitives), local lookup (uses existing `MeshBlobAdapter::fetch` for the local case), and stream-write logic.

### T-4: Receive-side reassembly and integrity check

**Where:** Same module.

**What:** On the requesting peer, packets arriving on the allocated transfer stream get reassembled (substrate handles fragmentation reassembly already; this layer just collects the reassembled chunk bytes). When the FIN arrives, verify the received content matches the requested hash (BLAKE2 or whichever hash the content-addressing uses). On match, store locally via existing `MeshBlobAdapter::store`. On mismatch, error.

If the request times out without a FIN, the stream gets torn down via the router's idle timeout, the fetch returns error, and the caller can retry against a different peer advertising the same chunk.

**Size:** ~150 LoC. Most of the work is integrity verification and timeout handling; reassembly is provided by the substrate.

### T-5: Directory transfer wrapper

**Where:** New `dataforts/dir/` module, or in the SDK layer above Datafort.

**What:** Walk source directory, build a manifest mapping relative paths to content hashes (with mode, symlink target). Store the manifest as a blob locally. Return the manifest's content hash as the root reference.

Receiver fetches the root hash (using T-2 through T-4), reads the manifest, then fetches each leaf chunk in parallel. Parallelism is bounded by a configurable max-concurrent-transfers (default 64-128); the router's fair scheduler handles the actual bandwidth allocation across the concurrent streams.

The wrapper owns:
- Manifest construction and parsing
- Path reconstruction with mode and symlink preservation
- Parallel transfer orchestration with bounded concurrency
- Progress reporting

**Size:** ~400 LoC.

## Total scope

Substrate-side work (T-1 through T-4): ~550 LoC.
Wrapper (T-5): ~400 LoC.
Tests for both layers: ~700 LoC.

Realistic effort: 2-3 weeks of focused work. The substrate work is small because the substrate primitives are already there — what's being added is the convention and the bridging code that uses them, not new transport infrastructure.

## What this plan does NOT add

- **No new wire format.** Uses the existing 68-byte `NetHeader` with `subprotocol_id` distinguishing transfer traffic.
- **No new dispatch path through CortEX.** Transfer rides on the router's stream-level dispatch via `subprotocol_id`, parallel to but separate from CortEX's RPC dispatch.
- **No new RPC mechanism.** Not an extension of nRPC. The control packet that initiates transfer is a single small packet, not an RPC call.
- **No new scheduler or bandwidth-management mechanism.** Reuses the `FairScheduler` exactly as it exists.
- **No new encryption, session, or auth machinery.** All inherited from the substrate's existing wire format.
- **No abuse of the capability fold.** The fold is used for discovery (which is what it's for) at fold-appropriate frequency (`causal:<hash>` advertisements are stable for as long as a peer holds a chunk). The transfer negotiation itself happens in router streams, not through fold mutations.
- **No new mechanism for replication.** RedEX continues to handle steady-state replication exactly as it does today. Transfer is an unrelated on-demand path that uses different primitives.

## Test phases

### Phase 1: Single blob transfer

Two paired nodes, content-address hash of a 1 MB blob. Sender holds the blob. Receiver requests via `MeshBlobAdapter::fetch`. Local miss triggers discovery, discovery finds the sender's `causal:<hash>` advertisement, requester allocates a stream and sends the control packet. Sender's handler receives it, validates auth, sends bytes back on the stream. Receiver reassembles, verifies hash, stores locally. Subsequent fetch is purely local (no peer traffic).

Pass criteria: First fetch under 50 ms on localhost-paired nodes. Bytes match byte-for-byte. Hash verification passes. Subsequent fetch under 5 ms.

### Phase 2: Many small files (`node_modules` scale)

Realistic `node_modules` from a real project: 25,000-40,000 files, 200-500 MB total, deep nesting, symlinks. Transfer between paired nodes via T-5 directory wrapper.

Pass criteria:
- Transfer completes without error.
- Reconstructed directory matches source byte-for-byte for every file, with modes and symlinks preserved.
- 200 MB / 30,000 files completes in under 30 s on localhost-paired nodes.
- Memory peak under 500 MB on both sides regardless of total size.
- **Throughput at high file count within 80% of throughput at low file count for equal byte volume.** This is the architectural property: 200 MB across 30,000 files should be within 20% of 200 MB across 200 files. The router's fair scheduler amortizing per-stream overhead is what makes this hold; if it doesn't hold, the architectural claim needs revision.

### Phase 3: Concurrent mixed workload

The bandwidth-fairness test. Start a directory transfer of a large `node_modules` between two peers. While the transfer is in progress, run other substrate operations across the mesh — tool calls, health checks, capability updates, smaller transfers between other peer pairs.

Pass criteria:
- The directory transfer completes at the expected rate (subject to its configured stream weight).
- Tool calls during the transfer return at normal latency, not slowed by the transfer's bulk traffic. The `FairScheduler` services higher-priority streams ahead of bulk transfer streams.
- Other concurrent transfers between different peer pairs proceed at their own fair rates, not starved by the first transfer.
- Aggregate utilization of the UDP socket stays high (fairness doesn't mean idle capacity); the scheduler just allocates fairly across active streams.

This phase is what demonstrates the substrate's mixed-workload correctness, which is the property that matters for real deployment. Engineers reading the demo see that the substrate handles concurrent workloads correctly, not just that it handles one workload fast in isolation.

### Phase 4: Cargo target directory (large-file stress)

The other extreme: fewer files but much larger (compiled artifacts, multi-hundred-MB binaries). Tests the substrate's fragmentation behavior, streaming for individual large chunks, sustained throughput on single-stream large transfers.

Pass criteria: 2 GB Cargo `target/` transfers in under 5 minutes on localhost-paired nodes. Memory under 500 MB. Resume on interruption works.

## What the demo materials must say

The framing matters as much as the numbers. The demo materials should anchor on these architectural claims, each supported by specific test results:

1. **Discovery and transfer at correct architectural layers.** Discovery happens through the capability fold's stable `causal:<hash>` advertisement convention at fold-appropriate frequency. Transfer happens through the router's stream-multiplexed scheduling, with the fold not involved per-chunk. Phase 2 results show transfer working at scale; phase 3 shows the fold isn't being churned by transfer activity.

2. **Per-stream scheduling preserves correctness under concurrent workloads.** Phase 3 explicitly tests this: bulk transfer doesn't starve interactive operations, and concurrent transfers compete fairly with each other. This is the property that distinguishes the substrate from naive "wrap a socket" alternatives.

3. **Substrate primitives doing the work, not new infrastructure.** The new code is convention and bridging — ~550 LoC substrate-side, ~400 LoC wrapper. Engineers reading the code can verify that the heavy lifting is the existing router, scheduler, encryption, session, and channel-auth machinery.

4. **Throughput scales with byte volume, not file count, at equal volume.** Phase 2's specific throughput-invariance criterion is what makes the architectural claim empirically observable. The fair scheduler's quantum-based per-stream allocation is what makes this hold structurally.

5. **No new layers above the transport.** Transfer is below CortEX, below nRPC. It rides on the router's stream primitives directly. The architectural simplicity is part of the claim.

## Order

1. **T-1** (subprotocol and stream conventions) — half a day.
2. **T-2** (discovery-to-stream bridge) — two to three days.
3. **T-3** (transfer handler with auth) — three days.
4. **T-4** (receive-side reassembly and integrity) — two days.
5. **Phase 1 test passes** before moving to T-5.
6. **T-5** (directory wrapper) — three to five days.
7. **Phase 2 test** at realistic `node_modules` scale.
8. **Phase 3 test** for concurrent mixed workload.
9. **Phase 4 test** for large-file stress.
10. **Demo materials** written against the actual measured numbers.

If any test phase reveals a problem with the architectural framing, the framing changes before the next phase runs. Better to discover the truth at phase 2 than at PR review.

## Open questions worth flagging but not blocking

- **Stream ID exhaustion under sustained heavy load.** The router uses u64 stream IDs; exhaustion isn't a near-term concern, but the allocation convention should be clear about reuse semantics.
- **Optimal stream weight for transfer vs other traffic.** Default probably equal-weight, configurable. Phase 3 results inform the default.
- **Backpressure semantics if the receiver is slow.** The router's per-stream queue depth handles this at the receiver side; the scheduler's quantum keeps the sender from monopolizing if other senders also want to send. Worth testing explicitly that slow receivers don't cause memory blowup on the sender.
- **What happens if a peer with the chunk disconnects mid-transfer.** Idle timeout fires, stream tears down, fetch returns error, caller retries against a different `causal:<hash>` advertiser. Worth testing this failure mode explicitly.
- **Interaction with RedEX replication.** A chunk being on-demand-fetched by one peer might also be in steady-state replication to other configured peers via RedEX. Both happen on different streams, scheduled fairly by the router. No conflict expected but worth verifying.

These are real questions but they're empirical — answered by running the tests, not by designing more upfront.
