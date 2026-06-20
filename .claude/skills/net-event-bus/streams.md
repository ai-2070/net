# Streams — Per-Peer, Ordered, Credit-Bounded

The bus and per-peer streams are two different surfaces with overlapping vocabulary. This file is when to reach for which, and what's load-bearing about the stream model.

## What this is

The **bus** is fan-out + transient + topic-oriented. You publish, anyone matching the channel listens, ring buffer drops on overload, no per-receiver state.

A **stream** is point-to-point + ordered (per stream) + credit-bounded + stateful. One sender, one peer, an explicit `(peer_node_id, stream_id)` identifier, and a byte-denominated credit window the receiver controls. You hold a handle. The backpressure path surfaces as a real error — `BackpressureError` — that you can branch on.

They serve different shapes. Don't pick one because the API names look familiar.

---

## Bus vs Stream — the cheat sheet

| Need | Use |
|---|---|
| "broadcast event X to whoever's listening" | Bus (channel or firehose) |
| "send a sequence of related messages to one specific peer, in order" | Stream |
| "send a large payload that needs flow control" | Stream (window-based) |
| "I don't know who the receiver is yet — find them" | Capability routing → then stream OR channel |
| "many small events, sub-millisecond, dropped is fine" | Bus with `DropOldest` |
| "I need backpressure that surfaces a real error" | Stream with `Reliable` (the bus only fails on `FailProducer` mode) |

---

## Stream basics

A stream is identified by `(peer_node_id, stream_id)`. Both are `u64`; `stream_id` is caller-chosen and opaque — no value range has reserved meaning. Each peer keeps its own sequence space per stream, so `(peer_A, 7)` and `(peer_B, 7)` are entirely distinct streams.

Reliability mode is per-stream and chosen at `open_stream`:
- `FireAndForget` — no NACKs, no retransmit, lowest latency. **Default** (`net/crates/net/src/adapter/net/stream.rs:95`).
- `Reliable` — selective NACKs, retransmit on loss (wired end-to-end as of v0.27), congestion-windowed. **No-loss, not in-order**: the substrate delivers events in *arrival* order plus a per-stream `seq`; a consumer that needs ordering reorders by `seq` itself (the blob-transfer engine does exactly this). `Reliable` means "every byte arrives", not "bytes arrive in send order" — the docstring at `stream.rs:49` states it: *"Reliable here means 'no loss', not 'delivered in order'."*

Credit window is in **bytes**, defaulting to `DEFAULT_STREAM_WINDOW_BYTES = 65_536` (64 KB) per stream (`net/crates/net/src/adapter/net/stream.rs:67`). `StreamConfig` exposes **four** knobs: `reliability`, `window_bytes`, `fairness_weight`, and `scheduled` (`stream.rs:87-154`). `scheduled: false` (the default) sends each packet straight to the socket; `scheduled: true` routes *originating* sends through the router's fair scheduler so `set_stream_weight` actually applies on this stream — use it for bulk transfers that shouldn't monopolize the link against other scheduled streams (it's what the v0.27 blob-transfer engine rides). Pass `window_bytes = 0` to disable backpressure entirely on that one stream — there are valid reasons (small bursty control channels) but it's an escape hatch.

Streams hang off the **mesh** transport's `MeshNode`. They are not a feature of the `Net` (event-bus) handle — `Net::builder()...build()` does not produce a `Mesh`. Calls to `open_stream` go through `Mesh` (Rust SDK) / `MeshNode` (TS / Python).

---

## Opening a stream — per SDK

### Rust

```rust
use net_sdk::mesh::Mesh;
use net::adapter::net::{StreamConfig, Reliability};

let mesh = Mesh::builder("0.0.0.0:9000", &psk)?.build().await?;
mesh.connect("203.0.113.10:9000", &peer_pubkey, peer_node_id).await?;
mesh.start();

let stream = mesh.open_stream(
    peer_node_id,
    /* stream_id */ 7,
    StreamConfig::new()
        .with_reliability(Reliability::Reliable)
        .with_window_bytes(65_536)
        .with_fairness_weight(1),
)?;
```

**Key facts:**
- `Mesh::open_stream(peer, stream_id, StreamConfig) -> Result<Stream>` (`net/crates/net/sdk/src/mesh.rs:752-761`). Handle type is `net::adapter::net::Stream`, re-exported.
- `StreamConfig::default()` = `{ reliability: FireAndForget, window_bytes: 65_536, fairness_weight: 1, scheduled: false }`. **Default reliability is `FireAndForget`** — opt into `Reliable` explicitly for NACKs + retransmit. Chain `.with_scheduled(true)` for fair-scheduled bulk sends.
- `open_stream` is sync (does not await). The SDK's `Mesh` is the wrapper around the core `MeshNode`; both share the same signature.

### TypeScript

```typescript
import { MeshNode } from '@net-mesh/sdk';

const mesh = await MeshNode.create({ bindAddr: '0.0.0.0:9000', psk });
await mesh.connect('203.0.113.10:9000', peerPubkey, peerNodeId);
await mesh.start();

const stream = mesh.openStream(peerNodeId, {
  streamId: 7n,
  reliability: 'reliable',
  windowBytes: 65_536,
  fairnessWeight: 1,
});
```

**Key facts:**
- `MeshNode.openStream(peerNodeId: bigint, config: StreamConfig): MeshStream` (`sdk-ts/src/mesh.ts:326`). The napi shape is `(peerNodeId, opts: StreamOptions)`; the SDK splays `streamId` into `opts`.
- `peerNodeId` and `streamId` cross the napi boundary as `BigInt`. A plain `number` throws — keypair-derived node ids routinely exceed `Number.MAX_SAFE_INTEGER`.
- `reliability` is a string tag: `'fire_and_forget'` (default) | `'reliable'`. `windowBytes` unset = 64 KB; `0` = backpressure disabled.
- Returned `MeshStream` is opaque — pass to `sendOnStream` / `closeStream`.

### Python

```python
from net_sdk import MeshNode

mesh = MeshNode(bind_addr="0.0.0.0:9000", psk="00" * 32)
mesh.connect("203.0.113.10:9000", peer_pubkey, peer_node_id)
mesh.start()

stream = mesh.open_stream(
    peer_node_id=peer_node_id,
    stream_id=7,
    reliability="reliable",     # default: "fire_and_forget"
    window_bytes=65_536,        # None / unset → 64 KB
    fairness_weight=1,
)
```

**Key facts:**
- `MeshNode.open_stream(peer_node_id, stream_id, *, reliability="fire_and_forget", window_bytes=None, fairness_weight=1) -> MeshStream` (`sdk-py/src/net_sdk/mesh.py:141-168`). PyO3 binding at `_net.NetMesh.open_stream` (`bindings/python/src/lib.rs:1409`).
- Python `int` is u64-native — no BigInt glue. `window_bytes=0` is the unbounded escape hatch; `None` inherits 64 KB.

### Go / C

The Go and C bindings do **not** expose the stream surface today. The poll-based API there covers `IngestRaw` / `Poll` against the bus only. If a Go or C consumer needs streams, that's a binding extension, not an existing call site.

---

## Sending — the three modes per SDK

All three modes accept the same payload shape (a batch of pre-serialized event bodies). The difference is what they do when the receiver's credit window is empty.

### Rust

```rust
use net_sdk::error::SdkError;
use bytes::Bytes;

let payloads = vec![Bytes::from(json_a), Bytes::from(json_b)];

match mesh.send_on_stream(&stream, &payloads).await {
    Ok(()) => {}
    Err(SdkError::Backpressure) => { /* drop, retry, or buffer */ }
    Err(SdkError::NotConnected) => { /* peer session is gone */ }
    Err(e) => return Err(e),
}
mesh.send_with_retry(&stream, &payloads, /* max_retries */ 8).await?;
mesh.send_blocking(&stream, &payloads).await?;
```

**Key facts:**
- `send_on_stream` returns `SdkError::Backpressure` (atomic — no events sent) or `SdkError::NotConnected` (peer gone). Everything else is `SdkError::Adapter(...)` (`net/crates/net/sdk/src/mesh.rs:662-691`).
- `send_with_retry` retries `Backpressure` only; transport errors and `NotConnected` return immediately.
- `send_blocking` retries `Backpressure` up to 4096 times (~13 min worst case) then surfaces. "Block until the network lets up, with a hard ceiling."

### TypeScript

```typescript
import { BackpressureError, NotConnectedError } from '@net-mesh/sdk';

const payloads = [Buffer.from(jsonA), Buffer.from(jsonB)];
try {
  await mesh.sendOnStream(stream, payloads);
} catch (e) {
  if (e instanceof BackpressureError)   { /* retry/buffer */ }
  else if (e instanceof NotConnectedError) { /* peer gone */ }
  else throw e;
}
await mesh.sendWithRetry(stream, payloads, 8);   // default 8
await mesh.sendBlocking(stream, payloads);
```

**Key facts:**
- `BackpressureError` / `NotConnectedError` are real classes — `instanceof` works (`net/crates/net/sdk-ts/src/mesh.ts:112-132`). The napi binding throws prefix-stable `Error`s; the SDK reclasses them.
- `sendWithRetry` re-throws transport errors and `NotConnectedError` immediately.

### Python

```python
from net_sdk import BackpressureError, NotConnectedError

payloads = [json_a.encode(), json_b.encode()]
try:
    mesh.send_on_stream(stream, payloads)
except BackpressureError:
    pass  # drop, retry, or buffer
except NotConnectedError:
    pass  # peer session gone
mesh.send_with_retry(stream, payloads, max_retries=8)
mesh.send_blocking(stream, payloads)              # releases the GIL
```

**Key facts:**
- `BackpressureError` / `NotConnectedError` are PyException subclasses defined via `pyo3::create_exception!` and re-exported from `net_sdk` (`sdk-py/src/net_sdk/mesh.py:39-43`). Use `isinstance` / `except`.
- `send_blocking` releases the GIL for the entire retry loop (`bindings/python/src/lib.rs:1499-1502`) — other threads keep running while you wait.

**Backpressure here means stream credit exhaustion, not bus backpressure.** The receiver hasn't issued enough `StreamWindow` grants to cover the next payload, and the send fails atomically (no events sent). Retry is safe. The bus's `Backpressure` is a different surface, only fires under `FailProducer` mode — see `runtime.md` § Errors.

---

## Receiving

Streams have no async iterator. You poll the underlying `MeshNode`'s shard buffers and demultiplex on `(peer, stream_id)` yourself.

### Rust

```rust
use net::event::StoredEvent;

loop {
    let events: Vec<StoredEvent> = mesh.recv_shard(0, /* limit */ 256).await?;
    for ev in events {
        // ev.raw is the payload bytes; parse, then route on stream_id
        // if you're multiplexing several streams onto the same peer.
    }
    if events.is_empty() {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}
```

**Key facts:**
- `Mesh::recv_shard(shard_id, limit)` and `Mesh::recv(limit)` (shard-0 shortcut) are the receive surface (`net/crates/net/sdk/src/mesh.rs:460-470`).
- These methods drain the **MeshNode's inbound shard buffer** — they're not stream-scoped. Inbound events from any peer / any stream land in the shards; you filter on the consumer.

### TypeScript / Python — high-level SDK gap

The high-level `MeshNode` wrappers in `@net-mesh/sdk` and `net_sdk` (Python) **do not currently expose a per-shard receive API**. The TS SDK's `MeshNode` class (`sdk-ts/src/mesh.ts`) has no `recv` / `recvShard` method; the Python SDK's `MeshNode` class (`sdk-py/src/net_sdk/mesh.py`) has no `recv` / `poll` method.

If you need to receive on a stream from TS or Python today, drop to the underlying napi / PyO3 binding:

- **TypeScript:** `mesh._native.poll(limit)` on the napi `NetMesh` class — `poll(limit: number): Promise<StoredEvent[]>` polls **shard 0 only** (`net/crates/net/bindings/node/index.d.ts:667`). There is no multi-shard `recvShard` in the napi surface today.
- **Python:** `mesh._native.poll(limit)` on the PyO3 `_net.NetMesh` — same single-shard limitation (`net/crates/net/bindings/python/src/lib.rs:1342-1363`).

This is a real divergence: Rust callers have multi-shard receive, TS and Python callers are single-shard via the binding. If your workload needs full shard coverage from TS/Python, file an issue against the SDK rather than working around it; faking it with multiple polls is fine until then.

Streams do not have an async-iterator shape on any SDK. Loop the poll yourself.

---

## Credit grants — the key mental model

Stream credit is **byte-denominated**, not packet-denominated. The sender starts with `tx_credit_remaining = window_bytes` and decrements by the payload size on each socket send via atomic CAS. The receiver replenishes the sender's credit by emitting `StreamWindow` grants over the dedicated subprotocol `SUBPROTOCOL_STREAM_WINDOW = 0x0B00` (`net/crates/net/src/adapter/net/mesh.rs:89,3415`).

The receiver controls flow. The sender does not negotiate. If `tx_credit_remaining` reaches zero, the next `send_on_stream` returns `Backpressure` until a grant arrives.

**Diagnostic implication:** persistent `Backpressure` means the receiver is slow or gone, not that the network is congested. The credit window is receiver-driven flow control, not congestion control — but a `Reliable` stream *also* paces itself to a Reno-style congestion window (slow-start, multiplicative decrease on NACK loss, reset-to-floor on timeout) gated by `can_send`, plus an adaptive RTO (RFC 6298 SRTT/RTTVAR with Karn's algorithm, clamped to `[10 ms, 2 s]`) — both added in v0.27 (`net/crates/net/src/adapter/net/reliability.rs`). So under loss a reliable sender back-pressures even with credit available; `FireAndForget` has no congestion state. If you see `Backpressure` for many seconds, look at the receiver, not the link.

`StreamStats` exposes the relevant counters (`net/crates/net/src/adapter/net/stream.rs:179-197`):

| Field | Meaning |
|---|---|
| `backpressure_events` | Cumulative `Backpressure` rejections since the stream opened. |
| `tx_credit_remaining` | Bytes of send credit left. `0` = next send is rejected. |
| `tx_window` | Configured initial window. `0` = backpressure disabled. |
| `credit_grants_received` | `StreamWindow` grants pulled in (sender side). |
| `credit_grants_sent` | `StreamWindow` grants emitted (receiver side). |

Pull these on a timer alongside `node.stats()` if you're tuning a sustained transfer.

---

## Reliability modes

| Mode | NACKs | Retransmit | Ordering | Use when |
|---|---|---|---|---|
| `FireAndForget` (default) | no | no | best-effort | telemetry, periodic heartbeats — drops are fine |
| `Reliable` | selective | yes | arrival-order + `seq` (no-loss, **not** reordered) | RPC-shaped requests, bulk transfer, anything where loss matters |

The choice is per-stream and locked in at `open_stream`. Two streams to the same peer can have different modes — that's a feature, not a quirk.

**`Reliable` is not in-order.** v0.27 corrected a docstring that previously claimed in-order delivery: the substrate delivers events in arrival order plus a per-stream `seq`, and the consumer reorders if it cares (the blob-transfer engine sorts by `seq`; nRPC frames its own order and is fire-and-forget). A general in-order delivery buffer is deferred — no consumer needs it. If your application assumes "Reliable ⇒ events arrive in send order", that assumption is wrong: sort by `seq` yourself.

---

## Idempotent open / close + epoch guard

`open_stream` is **idempotent**. Calling it twice with the same `(peer_node_id, stream_id)` returns a handle to the same underlying state — first open wins, later differing configs are logged and ignored. This is documented contract on all three SDKs.

`close_stream` is also idempotent. Calling it on a never-opened stream is a no-op. But it closes **eagerly** — it drops the retransmit window and any outbound packets not yet on the wire, which can strand a lost tail packet on a lossy link. After the last bytes of a `Reliable` send, prefer `MeshNode::close_stream_graceful(peer, stream_id, timeout)` (v0.27, `net/crates/net/src/adapter/net/mesh.rs:11005`): it waits until the reliability layer has no unacked packets (retransmit can still fill gaps) or `timeout` elapses, then closes. A `FireAndForget` stream has nothing tracked and drains instantly. Separately, when a reliable send exhausts its retries the substrate emits `SUBPROTOCOL_STREAM_RESET`, so a stalled receiver fails its pending read promptly instead of waiting out the 30-second transfer timeout. The SDK `Mesh` wrapper currently exposes only the eager `close_stream`; reach for the `MeshNode` primitive when you need the graceful path.

After **close + reopen** with the same id, the new state carries a different `epoch` (`net/crates/net/src/adapter/net/session.rs:68-73,212-216`). A stale handle held over from before the close fails at admission with `NotConnected` (epoch mismatch) — it does **not** silently start operating against the freshly-opened stream. This is the "no use-after-close" guarantee. If you cache stream handles across reconnect, reopen them; don't reuse stale handles.

---

## Worked example — 5 MB to one peer

A streams a 5 MB payload to B as 64 KB chunks on a `Reliable` stream. Each chunk consumes the default 64 KB credit window; B grants more before the next chunk lands.

```rust
// A side
let stream = mesh.open_stream(
    b_node_id, 0xCAFE,
    StreamConfig::new().with_reliability(Reliability::Reliable),
)?;
let payload: Bytes = Bytes::from(big_buffer); // 5 MB
for chunk in payload.chunks(60_000) {         // headroom under the 64 KB window
    let chunks = vec![Bytes::copy_from_slice(chunk)];
    mesh.send_with_retry(&stream, &chunks, 16).await?; // absorbs grant lag
}

// B side — drain shards, route on (peer, stream_id) if multiplexing
loop {
    let evs = mesh.recv_shard(0, 256).await?;
    if evs.is_empty() { tokio::time::sleep(Duration::from_millis(5)).await; continue; }
    for ev in evs { write_to_disk(ev.raw)?; }
}
```

Credit-grant math: `5 MB / 64 KB ≈ 80` round trips, each a `StreamWindow` packet B → A. Fine on a typical mesh link; on high-RTT links raise `window_bytes` rather than trying to pipeline another way.

---

## What streams are NOT

- **Not TCP.** Each `Reliable` stream has its own Reno-style congestion window + adaptive RTO (v0.27), but there is no *cross-stream* congestion control: two streams to the same peer don't share a window or a congestion state — they each have their own. And ordering is per-stream `seq`, not a single connection-ordered byte stream — sort by `seq` if you need order.
- **Not publish-subscribe.** One sender, one receiver. There is no fan-out. If you want fan-out, use a channel on the bus, or open one stream per receiver.
- **Not durable.** No replay; close = state gone. If you need durability, persist on the receive side or pair with RedEX / a Redis adapter at the application layer.

---

## Common pitfalls

- **Don't conflate `Backpressure` errors.** Stream `Backpressure` = per-stream credit window empty (atomic, retry-safe). Bus `Backpressure` only fires under `FailProducer` mode. Different surface, different retry envelope — `runtime.md` § Errors has the bus-side matrix.
- **`BackpressureError` / `NotConnectedError` are stream-only.** Bus `emit` / `publish` never throws them. Seeing them anywhere else means a wrong call site.
- **Don't share a `stream_id` across peers.** `(peer_A, 7)` and `(peer_B, 7)` are distinct streams by design. No global stream namespace.
- **Don't reuse stale handles after close + reopen.** Reopen the handle. The epoch guard fails closed with `NotConnected`.
- **Don't expect receive in TS / Python via the high-level SDK.** Drop to `mesh._native.poll(limit)` (single shard) until a wrapper lands. Rust has `Mesh::recv_shard` / `Mesh::recv`.
- **Don't pass `windowBytes: 0` casually.** It disables backpressure on that stream — escape hatch for tiny control channels, dangerous for bulk transfer.

---

## Cross-references

- `apis.md` § Cross-SDK gotchas — `BackpressureError` / `NotConnectedError` are mesh-stream-only. Same line; keep consistent if you edit either file.
- `runtime.md` § Errors — bus-side error matrix, including the bus's separate `Backpressure` semantics under `FailProducer`.
- `runtime.md` § Per-SDK shutdown — streams ride on `MeshNode`; closing the mesh closes all streams it owns.
- `concepts.md` § Transport — streams require the mesh transport. Memory / Redis / JetStream don't have a stream surface.
