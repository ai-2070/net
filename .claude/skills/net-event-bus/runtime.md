# Runtime — Lifecycle, Errors, Async Integration, Debugging

This is the file that prevents production fires. Read it when:
- You're writing or fixing a `shutdown` path.
- You need to handle errors from `emit` / `subscribe` / `publish`.
- You're integrating Net into an existing async runtime (axum, FastAPI, Express, etc.).
- The user reports "events aren't arriving" or "my test is flaky."

---

## Lifecycle

### The shutdown contract

Calling `shutdown` does three things, in order:
1. **Stop accepting new emits.** Subsequent `emit` calls return `Shutdown` errors (Rust) or raise binding errors in TS/Python sync paths (`Error` / `RuntimeError`).
2. **Drain the local ring buffer.** In-flight events finish their current dispatch cycle. Events that have not yet been picked up by a drain worker are lost.
3. **Disconnect transports.** Mesh sessions close, Redis/JetStream connections close, file handles flush.

Active `subscribe()` iterators on the same node terminate. Iterators on **other** nodes are unaffected — they just stop seeing new events from this publisher.

### Safe shutdown order in your application

```
1. Stop the producer (your business logic stops calling emit).
2. Optionally: small grace period (50–200ms) so in-flight emits land.
3. Cancel any subscribe loops you own (drop the iterator / break the loop).
4. Call node.shutdown() and await/wait.
5. Then exit the process.
```

If you skip step 1, late emits race with step 4 and surface as `Shutdown` errors. If you skip step 3, the iterator's poll task may panic on a torn connection. If you skip step 4, the mesh transport leaks UDP sockets and peers see your node as suspect (silent), not gracefully departed.

### Per-SDK shutdown specifics

| SDK | Call | Async? | Multi-call safety |
|---|---|---|---|
| Rust | `node.shutdown().await?` | Yes | **Consumes `self`** (single call enforced by ownership), but the work is reference-based — outstanding `EventStream` clones do **not** block shutdown. See "Rust: subscribe streams and shutdown" below. |
| TypeScript | `await node.shutdown()` | Yes | Idempotent — safe to call twice |
| Python | `node.shutdown()` or context manager exit | No | Idempotent |
| Go | `bus.Shutdown()` (typically `defer bus.Shutdown()`) | No | Idempotent |
| C | `net_shutdown(handle)` then stop using the handle | No | Calling twice is undefined — don't |

### Rust: subscribe streams and shutdown

`Net::subscribe()` and `subscribe_typed()` clone the inner `Arc<EventBus>` into the returned stream. `node.shutdown()` consumes `self` and delegates to `EventBus::shutdown_via_ref()` (`net/crates/net/sdk/src/net.rs:236-246`), which is **reference-based and idempotent** — it signals shutdown via internal flags rather than `Arc::try_unwrap`, so outstanding `EventStream` clones don't fail the call. Streams observe shutdown on their next poll and terminate cleanly.

Concurrent callers of `shutdown` are also safe: the second caller's CAS loses, then it spins on `is_shutdown_completed()` until the first caller finishes (10 s production deadline) and surfaces `AdapterError::Transient(...)` if that deadline elapses — never a silent `Ok` and never a panic.

This is a forgiving contract, but the recommended order in the previous section is still cleaner: stop emitting first, drop your subscribe loops, then call `shutdown`. You just won't hit a hard failure if a stream outlives the call.

### Tests

Use the **memory transport** in tests. Always `shutdown` in a tear-down hook. If a test asserts on subscriber output, set the subscriber up *before* the publisher emits — subscriptions are hot and you'll race.

---

## Errors

### Rust (`net_sdk::error::SdkError`)

Verified against `net/crates/net/sdk/src/error.rs`. The full enum:

| Variant | Meaning | Your action |
|---|---|---|
| `Shutdown` | Node was shut down before this call | Stop using the node. Don't retry. |
| `Ingestion(String)` | Local ring buffer rejected the event for a reason that doesn't map to a more specific variant | Fallback / future-proof bucket. The structured causes (`Sampled`, `Unrouted`, `Backpressure`, `Shutdown`) are routed to their own variants via `From<IngestionError>` — pre-`bugfixes-8` everything came through here as a string and callers had to substring-match. |
| `Sampled` | Event was deliberately dropped by a sampling / decimation policy | Retry is pointless. Producer should accept the drop or change the sampling rate. |
| `Unrouted` | No routable shard for the event (typically a topology-transient state — concurrent scale-down or shard still provisioning) | Retry once topology stabilizes. Back-off-and-retry on `Backpressure` semantics is the wrong remediation. |
| `Poll(String)` | Subscriber poll failed | Often transient. Caller decides retry policy. |
| `Adapter(String)` | Underlying transport (Redis, JetStream, mesh) returned an error | Check transport config and connectivity. |
| `Serialization(serde_json::Error)` | Could not serialize/deserialize the event | Fix the type. Indicates bug, not a transient error. |
| `Config(String)` | Builder rejected the configuration | Surfaces at construction. Fix before redeploying. |
| `NoMesh` | Asked for a mesh feature on a non-mesh transport | Either switch transport or stop calling the mesh feature. |
| `Backpressure` | Stream's per-stream send window full | Use `send_with_retry` (5–200ms exponential) or `send_blocking` helpers, or apply your own policy. |
| `NotConnected` | Peer session is gone | Reconnect or reroute at the application layer. Mesh layer will already be trying. |
| `ChannelRejected(Option<AckReason>)` | Publisher's ack rejected your subscribe/unsubscribe | Permission token issue or capacity limit. Inspect the reason. |
| `Traversal { kind, message }` | NAT-traversal optimization failed | **Never a connectivity failure** — fallback path still works. Log and ignore unless tuning. *Only present when the SDK is built with `features = ["nat-traversal"]`* — projects without that feature won't see this variant. |

`SdkError` is `#[non_exhaustive]`. Match arms must include a wildcard
(`_ => …` or a catch-all `Err(e) => …`) so a future variant addition is
a minor-version change rather than a breaking one. The two most recent
additions — `Sampled` and `Unrouted` — are exactly that case: they
moved out of `Ingestion(String)` so callers can dispatch on the cause
without substring-matching. Code that string-matched on
`Ingestion(...)` for "no shards" or "sampled" silently stops matching
on `bugfixes-8` and later.

`pub type Result<T> = std::result::Result<T, SdkError>;` — most APIs return this.

### TypeScript

Bus-level (`NetNode`) errors are not thrown for backpressure — `emit` / `emitRaw` return `null`, and `publish` / `emitBuffer` / `fire*` return `false`/short counts. **Check return values.** Construction and other unrecoverable problems arrive as plain `Error`.

`BackpressureError` and `NotConnectedError` (with `instanceof` support) are **mesh-stream-only** — they're thrown by `MeshNode.send(...)` / `MeshStream.send(...)` when a per-stream credit window is full or the peer session is gone. The `sendWithRetry` helper on `MeshNode` retries `BackpressureError` (5–200 ms exponential). None of this applies to bus emit.

### Python

Same shape as TS:
- Bus emit is silent on ring-buffer drops (visible only in `node.stats().events_dropped`). With `backpressure='fail_producer'`, the underlying PyO3 binding raises an exception — handle it at the call site.
- `BackpressureError` / `NotConnectedError` are **mesh-stream exceptions** raised by `MeshNode.send(...)`. The `send_with_retry` helper (5–200 ms exponential) and `send_blocking` (releases the GIL) live on `MeshNode`, not on `NetNode`.

### Go

Standard `error` returns. No typed error hierarchy. Inspect the message for context, or use `errors.Is` / `errors.As` if the binding wraps typed sentinel errors (check the package).

### C

Negative integer return codes from `NET_ERR_*`. Check `net.h` for the current set. `0` is success. `net_poll_ex` returning non-zero means no events were populated — don't read the result struct.

### Default policy

For any SDK: **`Backpressure` is the only error that's safe to retry blindly.** Everything else either indicates a bug (`Serialization`, `Config`), a permission/capacity issue (`ChannelRejected`), or a state change (`Shutdown`, `NotConnected`) that retrying won't fix.

---

## Async runtime integration

### Rust + tokio

The SDK requires a **tokio runtime context**. Calling `Net::builder().build().await` panics outside one. If your app already uses tokio (axum, actix-web, tonic, etc.), you're fine — the SDK uses the ambient runtime. If your app uses a different runtime (smol, async-std), you'll need to spawn a tokio runtime alongside it.

Subscribe streams are `Stream + Send + Unpin`. Move them into a `tokio::spawn` task or consume them from a request handler.

```rust
let stream = node.subscribe_typed::<MyType>(Default::default());
let task = tokio::spawn(async move {
    let mut s = stream;
    while let Some(event) = s.next().await {
        handle(event?).await;
    }
});
// later: task.abort() or graceful shutdown
```

### TypeScript / Node

The native binding (`@net-mesh/core`) runs work on its own threadpool (napi-rs). The Node event loop is not blocked by `emit` or `subscribe`. Async iterators yield to the event loop normally between events.

If you're inside a request handler (Express, Fastify, Hono), `emit` is sync and fast — call it directly. `subscribe` should be set up at app startup, not per-request, or you'll create one subscription per HTTP request.

`u64` fields (timestamps, sequence numbers, stream stats) cross the napi boundary as **`bigint`**, not `number`. Don't `JSON.stringify` them naively — use a replacer or convert with `String(value)`.

### Python

The `net_sdk` package is **synchronous**. The underlying PyO3 binding owns its own tokio runtime and **releases the GIL** during waits, so calling `emit` from one thread while `subscribe` iterates on another works.

- For sync apps (Flask, scripts): just use it directly.
- For async apps (FastAPI, asyncio): wrap blocking calls in `asyncio.to_thread(...)` or `loop.run_in_executor(None, ...)` so they don't block the event loop.
- `subscribe()` returns an `EventStream` that supports **both** sync (`for ... in`) and async (`async for`) iteration (`net/crates/net/sdk-py/src/net_sdk/stream.py:38-58`). The `async for` path still calls the same blocking `bus.poll(...)` per step — the GIL is released during the FFI wait, but the asyncio event loop is single-threaded and stalls until the call returns. Fine for low-latency polls; if event-loop responsiveness matters under load, consume the **sync** iterator inside `asyncio.to_thread(...)` (or a dedicated worker thread). Pick one iteration mode per stream instance; interleaving on the same instance is undefined.
- `send_blocking` (mesh) explicitly releases the GIL for the whole retry loop.

Python int handles `u64` natively (arbitrary precision) — no BigInt glue needed.

### Go

All SDK methods are **thread-safe and synchronous**. Idiomatic usage: spawn a goroutine for the poll loop.

```go
go func() {
    cursor := ""
    for {
        resp, err := bus.Poll(100, cursor)
        if err != nil { /* handle */; return }
        for _, ev := range resp.Events { handle(ev) }
        cursor = resp.NextID
        if !resp.HasMore { time.Sleep(10 * time.Millisecond) }
    }
}()
```

No async runtime required. `defer bus.Shutdown()` from `main`.

### C

Synchronous everything. Polling is your loop. If you want concurrency, spawn pthreads — all SDK functions are thread-safe.

---

## Debugging: "Why are my events missing?"

Run this checklist top-to-bottom. Stop at the first hit.

### 1. Is the subscriber actually running before the publisher emits?

Subscriptions are hot. If the publisher emits at T=0 and the subscriber subscribes at T=1ms, the subscriber may miss the event unless it is still in the publisher's local ring buffer when the subscribe lands. Common in tests and demos.

**Check:** add a log line before `emit` and before the first iteration of the subscribe loop. Confirm the subscribe-side logs first.

### 2. Are publisher and subscriber on the same transport?

A node with memory transport cannot talk to a node with mesh transport. A node with Redis transport using prefix `app1` cannot see events from one using prefix `app2`.

**Check:** print the transport config at construction on both sides. Compare.

### 3. Is the channel name an exact match?

Channel name matching is **string-equal**, not pattern-based. `sensors/temp` and `sensors/temperature` are different. Trailing slash matters. Whitespace matters.

**Check:** print the channel name on publish and on subscribe. Compare byte-for-byte.

### 4. Is the publisher dropping under backpressure?

Read `node.stats()` on the publisher. The fields:

| Rust | TS | Meaning |
|---|---|---|
| `events_ingested` | `eventsIngested` | Total events the bus accepted |
| `events_dropped` | `eventsDropped` | Events dropped due to backpressure |
| `batches_dispatched` | (not exposed) | Batches sent to the transport |

If `events_dropped > 0`, you're hitting backpressure. Either:
- Tune the producer down (rate-limit at source).
- Increase `buffer_capacity` / `ring_buffer_capacity`.
- Add more shards (`shards: N`).
- Switch backpressure mode to `FailProducer` so you learn synchronously instead of silently.

### 5. Is the subscriber falling behind?

If `events_dropped == 0` on the publisher but the subscriber sees gaps, the subscriber's local ring buffer is dropping. The subscriber side has the same `stats()` surface — check it on both ends.

### 6. Mesh transport: is the peer actually connected?

Check the mesh stats (varies per SDK — typically `mesh_peers()` or equivalent). If the peer is in the routing table but no recent heartbeat, it's `suspected` or `failed` — events to it are being routed elsewhere or dropped.

Common causes:
- UDP port not open in firewall.
- NAT in the way and `nat-traversal` feature not enabled.
- Bootstrap peer never resolved — mesh has no nodes to talk to.

### 7. Mesh transport: is encryption set up correctly?

If the handshake fails, the peer is "connected" at the IP level but no events flow. Look for `Adapter(...)` errors mentioning handshake or AEAD. Common cause: PSK mismatch between peers, or wrong public key in `mesh_peer_public_key`.

### 8. Subscriber is filtering out the events

In TS/Python channels, the subscribe filter on `_channel` is exact-match. If the publisher uses a custom validator that strips `_channel` or rewrites the payload, the filter may not match.

**Check:** subscribe with `node.subscribe()` (no filter) and confirm raw events arrive. If yes, the filter is the problem.

### 9. Schema / parse errors on the subscriber

Typed subscribe (`subscribeTyped<T>`, `subscribe(MyDataclass)`, `subscribe_typed::<T>()`) silently or loudly drops events that fail to parse. In Rust, the iterator yields `Result::Err`. In TS, the validator throws and the iterator may swallow it. In Python, the model constructor raises and the generator may swallow.

**Check:** subscribe raw (no type), log the bytes, then debug deserialization separately.

### 10. Rust only: did `shutdown()` return `Adapter("cannot shutdown: outstanding references exist")`?

Means a subscribe stream (or anything else holding the inner `Arc<EventBus>`) is still alive. The bus didn't drain — events from that point on are lost. See "Rust: subscribe streams hold the bus" above; drop the stream first.

### 11. Last resort: are you sure both processes are running the same SDK version?

Wire format is JSON, but transport-layer protocol details (framing, handshake versions, subprotocol IDs) can shift between Net releases. Match versions across all nodes that talk to each other.

---

## Quick reference: stats observability

```rust
let s = node.stats();
println!("ingested={} dropped={} batches={}", s.events_ingested, s.events_dropped, s.batches_dispatched);
```

```typescript
const s = node.stats();
console.log(`ingested=${s.eventsIngested} dropped=${s.eventsDropped}`);
```

```python
s = node.stats()
print(f"ingested={s.events_ingested} dropped={s.events_dropped}")
```

Pull these on a timer (every 1–10s) into your metrics system. `events_dropped` should normally be 0; sustained non-zero means the producer is out-running the consumer or transport.
