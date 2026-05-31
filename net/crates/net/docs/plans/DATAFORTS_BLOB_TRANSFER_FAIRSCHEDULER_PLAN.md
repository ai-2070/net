# Datafort blob transfer — FairScheduler stream data path (architecture correction)

**Status:** Planning. Supersedes the transport choice in `DATAFORTS_BLOB_TRANSFER_PLAN.md` and `DATAFORTS_BLOB_REPLICATION_AUTOROLE_PLAN.md` for the BULK path.
**Decision:** Move blob bytes over the substrate's **reliable stream + FairScheduler** directly — native UDP speed, bandwidth-managed. Do NOT carry bulk data through RedEX replication (a *replication* primitive) or nRPC (a *request/reply* primitive). Both were measured to fail at scale (`tests/redex_transfer_bench.rs`).

## Why the previous approach was wrong

The original stack assumed `FairScheduler → RedEX → CortEX → nRPC` for blob transfer. That inverts the layering:

- **RedEX is a replication primitive, not a transfer primitive.** Its job is keeping replicas of a chain consistent — leader election, heartbeats, windowed chain-sync. Benchmarked as a transfer: a heartbeat storm at ≥~500 per-chunk channels and outright non-delivery of ≥~1 MiB chunk-events (a chunk is one RedEX event, larger than the sync chunk budget). Neither `node_modules` nor Cargo `target/` transfers.
- **nRPC is a request/reply primitive, not a bulk transport.** Unary replies are one unfragmented datagram (~MTU); streaming responses are fire-and-forget (drop under burst). Fine for control messages, wrong for bytes.
- **FairScheduler + the reliable stream layer IS the transfer primitive** and was being skipped. It already gives: in-order lossless delivery (per-packet retransmit, SACK/NACK), MTU packing, tx-credit flow control, and weighted fairness so a transfer doesn't starve other mesh traffic — over the single shared `UdpSocket` at native speed.

## Corrected architecture

Two planes, cleanly separated:

- **Control plane (small, request/reply):** a tiny nRPC request — "send me blob `<hash>`; reply on stream `<id>`" — or the inverse push nudge. This is exactly what nRPC is for. No bytes ride it.
- **Data plane (bulk):** the holder opens a **reliable, fairness-weighted stream** to the requester and pushes the blob's bytes through `send_on_stream`, paced by a **bandwidth cap**. The requester reassembles in order and verifies the BLAKE3.

RedEX is not in the transfer path. (It remains the durability/replication layer for chains; the `ClaimLeadership` work from the role-by-intent plan stays as a legitimate replication feature, just not a transfer mechanism.)

## What exists vs. what we build

Already present (from the transport map):
- `MeshNode::open_stream(peer, stream_id, StreamConfig)` — `StreamConfig` carries `reliability` and `fairness_weight`.
- `send_on_stream(&Stream, &[Bytes])` / `send_with_retry` / `send_blocking` — MTU-pack + tx-credit window + retransmit; `Backpressure` when the window is full.
- Reliable mode: in-order, gap-free delivery (receiver rejects out-of-window seqs; SACK/NACK retransmit).
- FairScheduler weight propagated via `open_stream`.
- `BandwidthBudget` (token bucket, bytes/sec, classes) — exists in `redex/replication_budget.rs`, NOT wired to the stream layer.

To build:
1. **A bandwidth-capped chunked send loop** (the sender side of the transfer primitive).
2. **Receive-side reassembly + completion + verify** (a per-transfer inbound buffer; events arrive in-order per stream but the app must frame total length / completion).
3. **A transfer framing** (header: total length + content hash; then the bytes).
4. **A bandwidth limiter at the stream layer** — lift `BandwidthBudget`'s token bucket up so the send loop paces to a configurable bytes/sec ceiling (the user's "don't starve the whole process/computer").
5. **A control request** wiring the two planes.

## Plan

### F-1: bandwidth-capped reliable send

**Where:** new `dataforts/blob/transfer.rs` (or `adapter/net/transfer.rs` if it's broadly useful).

**What:** `send_bytes_reliably(node, peer, stream_id, bytes, weight, rate_bps)`:
- `open_stream` with `Reliability::Reliable` + `fairness_weight = weight`.
- Loop over `bytes` in ≤`MAX_PAYLOAD_SIZE` (8160 B) events; before each send, charge a token-bucket (`rate_bps` ceiling, ported from `BandwidthBudget`) — sleep/pace when out of tokens; `send_with_retry` to ride `Backpressure` from the tx-credit window.
- Frame: first event is a small postcard header `{ total_len, hash }`; subsequent events are raw bytes.

This gives: native UDP speed bounded by `min(fairness share, rate_bps)`, reliable in-order delivery, no replication/RPC overhead.

### F-2: receive-side reassembly

**Where:** same module + the inbound dispatch hook.

**What:** a per-(peer, stream) reassembly buffer: read the header event (total_len, hash), accumulate subsequent events in order until `total_len` bytes, verify BLAKE3, hand the assembled `Bytes` to the waiting fetch. Bounded by `total_len` (reject overlong). A receiver-side timeout fails the transfer for retry/failover. Decide the inbound surface: a dedicated stream-id range routed to a transfer dispatcher, or a `poll_shard`-style consumer.

### F-3: bandwidth limiter at the stream layer

**Where:** lift `BandwidthBudget` (token bucket, `refill_bps`, classes) out of `redex/` into a shared location usable by F-1. Per-transfer (or per-node-aggregate) bytes/sec ceiling so bulk transfer can't peg the NIC/CPU. Foreground/Background classes map naturally (a `node_modules` sync is Background; an interactive fetch is Foreground).

### F-4: control plane + wire into the blob adapter

**What:** a small request so a fetcher pulls a blob: requester sends `{ hash, my_stream_id }` (nRPC unary — tiny), holder runs F-1 to push the bytes on that stream, requester runs F-2 to reassemble. Wire `MeshBlobAdapter::fetch` miss → this transfer path (replacing the replication-mode A-2 path and the nRPC-data S-1 path as the BULK transport). Discovery still uses the `causal:<hex>` advertisement (S-3) to find a holder; the per-chunk advertisement-scaling limit still applies to DISCOVERY and is a separate concern (a directory transfer can discover the holder once and pull the whole manifest+leaves over streams).

### F-5: re-benchmark

Re-run `tests/redex_transfer_bench.rs` (or a transfer-path variant) against F-1..F-4. Acceptance: the configs that timed out on replication (500+ small files, ≥1 MiB chunks) transfer correctly, and throughput is a real number (MB/s bounded by the rate cap / fairness, not 0.3 MiB/s latency-bound). Capture throughput + (now feasible) the rate-cap behavior.

## What this supersedes / what stays

- **Supersedes (as the bulk transport):** the nRPC `blob.fetch_chunk` data path (S-1) and the replication-mode transfer (A-1 auto-leader-on-store, A-2 replication-fetch). Decision needed: keep the unary `fetch_chunk` RPC as a small-chunk / control fallback, or remove it. The replication auto-role wiring (A-1/A-2) should likely be **gated off the transfer path** (replication stays for chain durability, not blob movement).
- **Stays (orthogonal, reused):** `MeshBlobAdapter` local store; the `dataforts/dir` directory wrapper (manifest, tree reconstruction, paths/modes/symlinks); `causal:<hex>` discovery (S-3) for finding a holder; the `ClaimLeadership` transition (a real replication feature).

## Order

1. **F-3** (lift the bandwidth budget) — unblocks the cap in F-1.
2. **F-1** (capped reliable send) + **F-2** (reassembly) — the core primitive; test with a direct two-node byte transfer first (no adapter).
3. **F-4** (control + adapter wiring).
4. **F-5** (re-benchmark) — prove the scale + throughput the replication path couldn't deliver.

## Risks & open questions

- **Stream-id allocation / collision** for concurrent transfers between the same pair. Need a per-transfer stream-id scheme.
- **Receive-side surface.** Is `poll_shard` the right consumer, or does the transfer need a dedicated inbound dispatcher keyed on a stream-id range? (The map says inbound events go to shard queues; a transfer wants a contiguous byte stream, not shard-interleaved events.) This is the main unknown to resolve early in F-2.
- **Bandwidth cap granularity:** per-transfer vs. per-node aggregate. Per-node aggregate is what actually protects the box; per-transfer is simpler. Probably both (a node-wide budget + per-transfer fairness weight).
- **Many concurrent streams** (a directory's leaves): does opening hundreds of reliable streams have the same per-stream-overhead problem replication had? The stream layer is lighter (no heartbeat/election), but verify — possibly transfer a directory as ONE stream (concatenated, length-framed) rather than one stream per file.
- **Does `send_on_stream` bypass the FairScheduler on direct (non-routed) sends?** The map says direct local sends go straight to `socket.send_to`, bypassing the scheduler (which only governs *forwarded* packets). If so, fairness for a direct two-peer transfer comes from the tx-credit window + our rate cap, not the scheduler — confirm and adjust where the cap lives.
