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
- **Reliable in-order delivery.** A stream opened `Reliability::Reliable` delivers its byte sequence in order and gap-free (per-packet retransmit, SACK/NACK). This is what we build chunk-reassembly on.
- **Encryption.** ChaCha20-Poly1305 per-packet, keyed off the session. Already handles confidentiality and integrity at the wire level.
- **Session and authentication.** `session_id` in the header binds packets to authenticated sessions. The substrate's existing channel-auth determines whether a peer can publish/subscribe on a given channel.

The transfer demo's architectural claim is that all of this composes correctly for high-throughput, many-stream, fairness-preserved bulk byte movement. The new code is the thin convention layer that says "blob transfer uses streams this way" — not new transport, not new scheduling, not new auth.

## Verified substrate facts + two corrections (2026-05-30)

Reading `router.rs`, `protocol.rs`, `mesh.rs`, `session.rs` against the claims above, two assumptions don't hold as written and shape the build:

1. **Fragmentation is NOT implemented — only reserved.** `protocol.rs` has the header fields (`frag_flags`, `fragment_id`, `fragment_offset`) + a `with_fragment()` builder, but nothing in the data path fragments or reassembles (the builder is only used in a unit test; `send_on_stream` MTU-packs into one packet and rejects oversize). **So the transfer chunks the blob into ≤`MAX_PAYLOAD_SIZE` (8108 B) pieces itself and reassembles by concatenating the reliable stream's in-order events until FIN.** The reliable stream does the hard part (ordering, retransmit); the transfer just splits and concatenates.

2. **The FairScheduler arbitrates only RELAYED traffic, not originating sends.** The router send loop sends only what's `enqueue`d to the scheduler; `route_packet` enqueues only the *forward* path; originating `send_on_stream` calls `socket.send_to` **directly**, bypassing the scheduler. So `open_stream`'s `set_stream_weight` governs a stream only when this node *relays* it — for a direct two-peer transfer (the demo topology) the scheduler is not in the path at all. **Fix: T-0.5 adds an opt-in `StreamConfig.scheduled` flag that routes a stream's originating sends through the scheduler**, so bulk-transfer streams get real fairness while control/RPC/replication keep the direct path.

**Bandwidth note:** the FairScheduler gives *fairness* among scheduled streams, not an absolute bytes/sec ceiling. "Don't starve the box" for bulk is covered by fairness (interactive sends stay direct and bypass the bulk queue) + the tx-credit window; a hard rate cap (token bucket) is a separate, optional pacing step if ever needed — not in this plan's core.

## What needs to be built

Six small pieces, in order.

### T-0.5: Route opt-in scheduled streams through the FairScheduler

**Where:** `stream.rs` (`StreamConfig`), `mesh.rs` (`send_on_stream`).

**What:** add `StreamConfig.scheduled: bool` (default `false`). When set, `send_on_stream` enqueues each built packet to `router.scheduler()` (a `QueuedPacket { data, dest, stream_id, priority }`) instead of calling `socket.send_to` directly; the router's existing send loop dequeues and sends it, applying the per-stream weight `set_stream_weight` already configures. tx-credit is acquired *before* enqueue (flow control unchanged); a full scheduler queue maps to `StreamError::Backpressure` (same as `WindowFull`, so `send_with_retry` handles it); per-stream queues are FIFO so reliable in-order holds. Default-`false` keeps every existing caller (nRPC streaming, etc.) on the direct path — zero blast radius outside opted-in transfer streams.

Cost: one `Bytes` copy per packet on the scheduled path (the build pool buffer is reused after the call). Acceptable for bulk; optimizable later.

**Size:** ~30 LoC + tests.

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

**What:** A handler registered for `SUBPROTOCOL_BLOB_TRANSFER` that receives the control packet on a new stream, validates authorization, looks up the chunk in local storage, and sends it back on the same stream — **chunked into ≤`MAX_PAYLOAD_SIZE` reliable events (the transfer splits; the substrate does NOT fragment, per the corrections above) and terminated by a FIN.**

**Authorization model — LOCKED (2026-05-31): possession-of-hash.** The original framing ("requester subscribed to a channel that authorizes read; channel-auth gates this") does not fit a **content-addressed** transfer: the request names a 32-byte BLAKE3 hash, and a blob may belong to many channels or none, so there is no single channel to gate on. The chosen model is **possession-of-hash is the capability**: a peer that presents a valid content hash may fetch the bytes that hash to it. The 256-bit digest is an unguessable bearer token. Two substrate guarantees backstop it, both already enforced: (1) the handler runs only for an AEAD-decrypted packet on an established session with a resolved `from_node ≠ 0` (no unauthenticated peer reaches it); (2) the reply goes via `open_stream(requester)`, which requires `requester` to be a connected peer, so bytes never flow to an unknown origin. **Caveat (by design):** the hash is a bearer token — anyone who learns it can fetch from any holder; sensitive-content callers must treat the hash as a secret or layer channel/capability auth above this transport. Documented at `BlobTransferEngine::on_request`.

The stream is opened **scheduled (T-0.5) + weighted**, so its sends ride the fair scheduler's per-stream allocation. Weight can be high or low depending on whether the transfer should be aggressive or background.

**Size:** ~200 LoC including the handler registration, auth check (uses existing primitives), local lookup (uses existing `MeshBlobAdapter::fetch` for the local case), and the chunked stream-write loop.

### T-4: Receive-side reassembly and integrity check

**Where:** Same module.

**What:** On the requesting peer, the reliable transfer stream delivers events in order; this layer **concatenates them in arrival order into the chunk buffer** (the substrate does NOT reassemble — it just guarantees order + gap-free, per the corrections). When the FIN arrives, verify the assembled content matches the requested hash (BLAKE3, the content-addressing hash). On match, store locally via existing `MeshBlobAdapter::store`. On mismatch, error. Bound the buffer by an expected-size cap so a misbehaving sender can't OOM the receiver.

If the request times out without a FIN, the stream gets torn down via the router's idle timeout, the fetch returns error, and the caller can retry against a different peer advertising the same chunk.

**Size:** ~150 LoC: an in-order concatenation buffer keyed on the transfer stream, integrity verification, and timeout handling.

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
- **No new scheduler.** Reuses the `FairScheduler` as-is; T-0.5 only adds an opt-in `StreamConfig.scheduled` flag that routes a stream's *originating* sends through it (today only relayed packets are scheduled). No new scheduling algorithm, no new bandwidth mechanism — just letting originating transfer streams participate in the fairness that already exists.
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

#### Phase 2 — MEASURED (2026-05-31, `tests/dir_transfer.rs`)

The architectural claim **holds**; running at scale surfaced two issues, one fixed and one substrate-level.

- **Throughput-invariance: CONFIRMED.** `bench_throughput_invariance` — equal 8 MiB volume, few-large (2×4 MiB) vs many-small (1024×8 KiB): ratio **1.28** (target ≥ 0.80). Throughput tracks *volume, not file count*; many-small is faster because more concurrent streams fill the pipe better. The fair scheduler amortizes per-stream overhead as claimed.
- **Transfer rate holds with file count.** `bench_nodemodules_scale` — a 1244-file deep-nested tree reconstructs byte-for-byte at ~1900 files/s, vs ~2100 files/s at 332 files.
- **FIXED — transfer streams leaked.** Each transfer left its stream open on both sides (reclaimed only at the 300 s idle timeout), so a directory pull accumulated one live stream per chunk. Now `serve_chunk` closes the reply stream after send and the engine closes the receive stream on completion (commit `c38fa1af1`). A FIN + ack-tracked close that survives mid-transfer loss on a lossy link is a follow-up.
- **Concurrent large-file transfers — diagnosed + fixed.** A fan-out sweep (`tests/transfer_concurrency.rs`) exposed two distinct bugs the single-stream tests never hit:
  1. **Reassembly assumed in-order delivery (commit `2bfd35c52`).** The substrate's `on_receive` accepts out-of-order sequences (dedup + SACK, no ordering), and the transfer divert delivered events in arrival order — so under any reordering a data packet could be decoded as the header ("bad header: varint…") or silently corrupt the concatenation. Fixed: the engine now reorders by the reliable sequence number (header = seq 0, data = seq 1..N), buffering out-of-order packets and dropping dups / far-future seqs.
  2. **tx window exceeded the retransmit window (commit `b96606767`).** A reliable stream only tracks its most recent `ReliableStream::DEFAULT_MAX_PENDING` (32) unacked packets for retransmit. The earlier 5 MiB transfer window let ~640 packets fly before the receiver's grants caught up, so under a slow (contended) receiver an early drop aged past the 32-packet window, got evicted, and was unrecoverable → 30 s timeout. Fixed by sizing the window to the retransmit window (`DEFAULT_MAX_PENDING × DATA_FRAME_BYTES`): every in-flight packet stays recoverable, AND aggregate in-flight shrinks enough that the recv buffer no longer overflows in the first place. Throughput is unchanged (it was loopback-latency-bound, not credit-bound). `tests/transfer_concurrency.rs` now passes k=2..8 concurrent 4 MiB transfers (was: k≥4 failed).
  3. **Aggregate in-flight budget (commit `0006e5d6e`)** — `fetch_dir` also caps total in-flight bytes (8 MiB) as defence-in-depth for high fan-out; with the corrected per-stream window it's belt-and-suspenders rather than load-bearing. `directory_transfer_many_large_files` (6 × 4 MiB) transfers cleanly at the default fan-out.
  - **Remaining (lossy links) — the substrate doesn't retransmit, full stop.** Investigated for the "FIN + ack close" follow-up and found the premise invalid: the reliable-stream machinery *registers* packets for retransmit (`on_send`) and *can* build NACKs / list timed-out descriptors (`build_nack`, `on_nack`, `get_timed_out`), but **nothing in production calls them** — there is no `spawn_retransmit_loop`, the receive path never emits a NACK, and those three methods are referenced only in `reliability.rs` tests. So a reliable stream recovers nothing: a lost packet leaves a permanent gap. On localhost/LAN this is invisible because the corrected window keeps aggregate in-flight under the recv buffer (no drops); on a lossy WAN any drop → 30 s timeout. The fix is **wiring the retransmit driver** (receiver emits a NACK on gap-detect; sender's NACK handler + a timeout tick rebuild from the retransmit descriptors and resend) — a substrate-level feature that benefits *every* reliable stream (nRPC streaming included), well beyond blob transfer. A FIN + ack-tracked close only matters *after* that exists. Until then, transfer is reliable on loss-free paths only. Callers issuing many concurrent `transfer_fetch_chunk` directly (not via `fetch_dir`) must bound their own concurrency.
- **STORE memory wall — FIXED (was the 30k-file blocker).** `MeshBlobAdapter` opens one RedEX chunk-file per chunk, and each used to pre-reserve `RedexFileConfig::max_memory_bytes` (default 64 MiB) up front (`HeapSegment::with_capacity`); thousands of small chunks blew the commit limit (a 64 MiB alloc failed at ~1.3k files). Fix: `max_memory_bytes` is only an *initial reservation hint* (the grow-only segment extends past it to the 3 GB hard limit), so `chunk_file_config` now defaults a chunk file's reservation to **0** — a write-once chunk's single append sizes the segment to its content. N chunks cost ≈ Σ(content) instead of N × 64 MiB. **Verified:** `bench_nodemodules_scale` now moves a **12,430-file / 27.4 MiB** tree byte-for-byte on the default (un-capped) config in ~8.3 s (~1490 files/s), where it previously OOM'd at ~1.3k files. The full 25-40k range is now a question of patience, not memory.

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

1. **T-0.5** (opt-in scheduled-stream routing) — half a day; unit-test that a scheduled stream's sends go through the scheduler and a default stream's don't.
2. **T-1** (subprotocol and stream conventions) — half a day.
3. **T-2** (discovery-to-stream bridge) — two to three days.
4. **T-3** (transfer handler with auth) — three days.
5. **T-4** (receive-side reassembly and integrity) — two days.
6. **Phase 1 test passes** before moving to T-5.
7. **T-5** (directory wrapper) — three to five days.
8. **Phase 2 test** at realistic `node_modules` scale.
9. **Phase 3 test** for concurrent mixed workload.
10. **Phase 4 test** for large-file stress.
11. **Demo materials** written against the actual measured numbers.

If any test phase reveals a problem with the architectural framing, the framing changes before the next phase runs. Better to discover the truth at phase 2 than at PR review.

## Substrate follow-up: on-demand chunk-segment sizing — DONE

**Problem.** The blob store opens one RedEX chunk-file per chunk, and `RedexFile::new` pre-reserved `min(max_memory_bytes, 64 MiB)` via `HeapSegment::with_capacity`. So N chunks reserved `N × 64 MiB` up front regardless of content. At node_modules scale (tens of thousands of tiny chunks) that's hundreds of GiB → OOM. Capping `max_memory_bytes` lower only traded the ceiling for a floor (`N × cap`).

**Fix (landed).** `max_memory_bytes` turned out to be *only* the initial reservation hint — `HeapSegment` is grow-only and extends past it to the 3 GB hard limit (the field is read at exactly one site, `file.rs:110`). So no `redex/segment.rs` change was needed: `MeshBlobAdapter::chunk_file_config` now defaults the reservation to **0**, and a write-once chunk's single append sizes the segment to its content. `with_chunk_file_max_memory_bytes` is retained for operators who want to pre-reserve uniformly-large chunks.

**Verified.** `bench_nodemodules_scale` moves a 12,430-file / 27.4 MiB tree byte-for-byte on the default config (~8.3 s, ~1490 files/s) — previously OOM'd at ~1.3k files. 492 lib blob tests + the transfer/dir/fairness suites pass unchanged. The general `RedexFileConfig` default (64 MiB) is left as-is (append-heavy event logs may want the prealloc); only the write-once chunk path opts out.

## Open questions worth flagging but not blocking

- **Stream ID exhaustion under sustained heavy load.** The router uses u64 stream IDs; exhaustion isn't a near-term concern, but the allocation convention should be clear about reuse semantics.
- **Optimal stream weight for transfer vs other traffic.** Default probably equal-weight, configurable. Phase 3 results inform the default.
- **Backpressure semantics if the receiver is slow.** The router's per-stream queue depth handles this at the receiver side; the scheduler's quantum keeps the sender from monopolizing if other senders also want to send. Worth testing explicitly that slow receivers don't cause memory blowup on the sender.
- **What happens if a peer with the chunk disconnects mid-transfer.** Idle timeout fires, stream tears down, fetch returns error, caller retries against a different `causal:<hash>` advertiser. Worth testing this failure mode explicitly.
- **Interaction with RedEX replication.** A chunk being on-demand-fetched by one peer might also be in steady-state replication to other configured peers via RedEX. Both happen on different streams, scheduled fairly by the router. No conflict expected but worth verifying.

These are real questions but they're empirical — answered by running the tests, not by designing more upfront.
