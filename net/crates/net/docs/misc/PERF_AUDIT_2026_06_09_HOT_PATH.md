# Performance Audit — Hot Path Survey (2026-06-09)

Source: code inspection of the mesh packet path (`src/adapter/net/`), the in-process
bus ingest path (`src/bus.rs`, `src/shard/`, `src/timestamp.rs`), and a sweep of the
existing perf plan docs to establish what is already identified, shipped, or gated.
Numbers from `BENCHMARKS.md` (2026-04-27, M1 Max + i9-14900K) and the prior audits are
used as the baseline; per-item costs below marked *est.* are reasoned from the code,
not re-measured.

> **Status: findings only (2026-06-09).** No fixes applied. This is a survey answering
> "is there meaningful hot-path headroom left, and where" — the answer is *yes, but
> almost entirely on the mesh/nRPC wire path, not the bus core*. Several of the largest
> levers are already designed and sitting at decision gates (see §1, §2); this doc adds
> two genuinely new findings (§4, §5) and consolidates the rest in one place.

The headline conclusion, consistent with `PERF_AUDIT_2026_05_19_NRPC.md` and
`docs/plans/NRPC_FLAMEGRAPH.md`: **the system is syscall- and wakeup-bound, not
compute-bound.** Flamegraph attribution: 51% wake/scheduling, 22% transport syscalls,
~5% AEAD. Compute-side micro-optimization of the bus core is done (§7); the remaining
headroom is in collapsing syscalls, eliminating wakeups, and one unconditional RX-path
allocation.

Recommended order of attack is at the bottom.

---

## 1. (Highest leverage) Recv-loop batching is built but parked — run the c128 gate, land the channel-hop gap-fix first

**Status of the work.** `batched-ingress` (Linux `recvmmsg`, `MAX_BATCH_SIZE = 64`) is
implemented through Stage 5 of `docs/plans/NRPC_RECV_LOOP_BATCHING_PLAN.md` but is
**default-off** behind both a Cargo feature and a runtime flag
(`MeshNodeConfig::batched_ingress`). Flipping the default is parked pending the c128
measurement.

- Default RX path: one `recvfrom` per packet — `PacketReceiver::recv()` →
  `socket.recv_buf_from()` (`src/adapter/net/transport.rs:396-401`).
- Batched RX path: `recvmmsg` wrapper (`src/adapter/net/linux.rs:316,422,626-647`),
  `BatchedPacketReceiver` dedicated OS thread + bounded mpsc
  (`src/adapter/net/transport.rs:303`), wired into the adapter at
  `src/adapter/net/mod.rs:817`. Gating at `src/adapter/net/mesh.rs:3287-3304`.

**Why this is the top item.** `NRPC_QPS_CONCURRENCY_SCALING_PLAN.md` (Phase 0,
diagnosed) shows the single shared recv loop is the structural concurrency ceiling:
c1→c16 scales only ~4×, worker-count sweeps don't move it, and recv syscalls + wakeups
account for 22% of CPU. The plan's own conclusion was "no safe in-scope lever" with
batched recv named as one of the two real levers.

**Gap-fix to land *before* measuring** (plan gap-fix #1, ~40 LoC): the batched receiver
currently forwards packets **one at a time** through the mpsc channel, so a 64-packet
`recvmmsg` batch degrades into 64 channel sends — the syscall collapse survives but the
wakeup collapse (the larger cost, per the flamegraph) is thrown away. Change the channel
payload to `Vec<(Bytes, SocketAddr)>` (one send per batch). Measuring without this
understates the design and could fail the gate for the wrong reason.

**Known risk the gate must check** (plan gap-fix #2 / measurement criteria): the
cross-thread hop taxes *every* inbound packet, including lone unary nRPC at c1 — verify
unary p50/p99 doesn't regress, and consider replacing the 1 ms busy-poll idle with a
kernel-blocking poll before default-on.

---

## 2. Ack-piggyback wire change — the only lever on the unary QPS ceiling

Per `NRPC_FLAMEGRAPH.md`, grants are the sole ACK and cost 4–6 spawns per round trip
(1–3 µs each, 5–15 µs total) on a path that is wake-latency bound. Skipping grants on
unary was evaluated and correctly rejected as unsafe (grants are the retransmit
backstop). The safe version — piggybacking acks on response traffic — is
design-complete in `NRPC_ACK_PIGGYBACK_PROTOCOL_PLAN.md` but unimplemented.

This is the path from the ~70K QPS plateau toward the 150–200K target
(`PERF_AUDIT_2026_05_19_NRPC.md`); nothing smaller in scope reaches it. It is a wire
format change, so it carries the usual cross-binding compat work (golden vectors,
`bindings/{node,python}` compat tests).

---

## 3. Crypto SIMD opt-in is being left on the table in deployments

Carried forward from `PERF_AUDIT_2026_06_08_BENCHMARK_WINS.md` §1 (documented opt-in,
deliberately not enforced in committed config): default builds compile
ChaCha20-Poly1305 to the portable backend, a fixed ~1.0–1.1 µs per packet independent
of payload — the bulk of the ~5 µs transport+crypto floor. `RUSTFLAGS="-C
target-feature=+avx2"` recovers 5–10× on the fixed cost (64B encrypt: 1.13 µs →
~100–200 ns).

The committed-config decision stands; the open question is **deployment-side**: the
published artifacts (Python wheel, Node prebuilds, release binaries) currently ship the
portable path. Worth deciding per-artifact whether to build with arch feature flags (or
fat/multiarch dispatch) rather than leaving the 5–10× as a footnote only source builds
can claim. Cipher caching itself is correct — instance per session, key schedule
reused, stack nonce from a shared counter (`src/adapter/net/crypto.rs:596-649`,
`pool.rs:679-705`, `session.rs:129`); the fixed cost is purely the backend selection.

---

## 4. NEW: per-event `String` allocation on the inbound hot path

The **only unconditional heap allocation** on the RX fast path
(`src/adapter/net/mesh.rs:5068-5070`):

```rust
let mut event_id = String::with_capacity(24);
let _ = write!(event_id, "{}:{}", seq, i);
queue.push(StoredEvent::new(event_id, event_data, seq, shard_id));
```

One `String` alloc + format per inbound event, every event, on a path where decrypt is
in-place (no alloc when `Bytes` refcount == 1) and `EventFrame::read_events()` slices
without copying. *Est.* ~100–150 ns per event; packets often carry multiple events, so
this stacks per packet. At high pps it is also pure allocator pressure.

**Fix direction** (no protocol change): `StoredEvent::event_id` is `seq:index` by
construction — store it as a `(u64, u32)` pair (or inline-array small string) and
format lazily only where something actually renders it. Touches `StoredEvent` and its
consumers; wire format unaffected.

---

## 5. NEW: unary sends bypass the shipped sendmmsg batching; routed-path copies

- **Send batching only covers scheduled streams.** Phase 1 of
  `NRPC_SEND_LOOP_BATCHING_PLAN.md` shipped (62× syscall collapse on concurrent
  scheduled streams via per-destination `send_batch` / `sendmmsg`,
  `src/adapter/net/transport.rs:710-715`, `linux.rs:169-220`). But unary RPC sends go
  directly out via `socket.send_to` (`src/adapter/net/mesh.rs:8342`), one syscall per
  packet, and the event publish path flushes `current_batch` to a per-packet
  `send_to` as well (`mesh.rs:5531-5558`). Encryption is amortized across the batch;
  the syscalls are not. Expect this to matter only after §1/§2 move the bottleneck.
- **Routed/forwarded packets allocate per hop**: fresh `BytesMut` + two memcpys to
  splice the forward header (`mesh.rs:3561`, similar at `3627`, `3784`, `5627`,
  `5655`). Cold relative to the direct path; acceptable trade-off today, listed so
  it's on record if relay-heavy topologies become a profile.
- **Retransmit path deep-copies** (`mesh.rs:4286`, `Bytes::copy_from_slice` per rebuilt
  packet) and sends sequentially inside a spawn (`mesh.rs:4292-4295`) — loss-path only,
  fine in steady state.

---

## 6. Smaller already-scoped nRPC items (from `NRPC_FLAMEGRAPH.md`, not yet landed)

T1.1 (grant coalescing) and T1.2 (response via `publish_to_peer`) landed. Still open:

| item | est. win | note |
|---|---|---|
| T3.4 drop `catch_unwind` on dispatch | 100–300 ns/call | cheap, low-risk |
| T2.2 `encode_into(BytesMut)` | 200–400 ns @ 1 KiB | kills an encode-side copy |
| T2.1 drop fold `Mutex` | helps c128 ceiling | medium risk, re-bench after §1 |

Also still pending from `PERF_AUDIT_2026_05_28_CAPABILITY.md`: fix #3 (compact codec
for capability serialize — wire-format gate) and #4 (`OnceCell` projection cache).

---

## 7. Bus core: done — verified, leave it alone

The ingest path was audited end-to-end and is as tight as its design allows. For the
record, the per-event cost structure on the `ingest_raw` fast path (~18 ns measured):

| component | cost (est.) | verdict |
|---|---|---|
| shutdown guard, `fetch_add`/`load` SeqCst pair + RAII decrement (`bus.rs:909-916`) | ~10 ns | only candidate, see below |
| shard select, Lemire multiply-shift (`shard/mod.rs:473-501`) | 1–2 ns | optimal |
| timestamp, TSC + monotonic CAS, per-shard generator (`timestamp.rs:100-152`) | 6–12 ns | correct & contention-free; the in-loop TSC re-read is deliberate (drift fix) |
| ring buffer push, 2 atomics, `CachePadded` head/tail (`shard/ring_buffer.rs:233-261`) | 2–3 ns | optimal; `pop_batch_into` amortizes the consumer side to ~0 |
| `Bytes` refcount bump (`event.rs:238-249`) | 2–5 ns | inherent to zero-copy |
| stats counter, Relaxed `fetch_add` (`shard/mod.rs:154-156`) | ~1 ns | negligible |

Two footnotes, neither urgent:

- **Shutdown-guard ordering** (`bus.rs:909-916`): if the "no producer mid-push when
  drain workers sweep" contract survives an Acquire/Release downgrade of the SeqCst
  pair, ~5–10 ns/event comes back — but SeqCst is plausibly load-bearing here; a loom
  model (`tests/loom_models.rs` infrastructure already exists) should decide, not
  intuition.
- **Per-push metrics when a collector is armed** (`shard/mod.rs:157-159`):
  `Instant::now()` + `elapsed()` + two records per push, *est.* 50–200 ns — fine as an
  opt-in instrument, just never arm it on a production ingest path; consider sampling
  (1-in-N) if it's ever needed live.

Also verified clean on the mesh RX path, listed so nobody re-audits them: session
lookup is 2 uncontended DashMap gets + a Relaxed cached-node-id load
(`mesh.rs:3605-3617`, `session.rs:165-170`); the replay-window mutex is
correctness-required and already fused with the admit counter; per-packet tracing is
error-path only; task spawns are confined to control/recovery paths (NACK, NAT,
migration — `mesh.rs:4120,4292,4567`) and don't fire in steady state.

---

## Explicit non-goals (don't spend time here)

- **AEAD algorithm swap** (ChaCha20-Poly1305 → AES-GCM): crypto is ~5% of CPU; saves
  1–2 µs at best and costs the portable-everywhere property. Ruled out in
  `PERF_AUDIT_2026_05_19_NRPC.md`; §3 (SIMD backend) captures the actual win.
- **Lock redesign**: replay window and per-shard mutexes are correct and uncontended.
- **Ring buffer / shard mapper / timestamp generator**: done (§7).
- **Moving decrypt off the recv loop**: ~5% win, not worth reordering flow control
  (already rejected in the QPS plan).

---

## Recommended order of attack

1. **§1 gap-fix #1 (batched channel hop, ~40 LoC), then run the c128 measurement** —
   unblocks the recv-loop batching default and is the gate for the structural ceiling.
2. **§4 event-id allocation** — small, unconditional, no protocol change; can land
   independently any time.
3. **§2 ack-piggyback** — the big unary lever; schedule as its own wire-change effort
   with cross-binding compat.
4. **§3 deployment decision on SIMD artifacts** — a build-pipeline decision, not code.
5. **§6 micro-items (T3.4, T2.2)** — opportunistic; re-bench T2.1 only after §1 lands.
