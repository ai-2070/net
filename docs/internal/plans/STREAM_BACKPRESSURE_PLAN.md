# Stream Backpressure — make the signal real, keep daemons simple

## Status

**v1 + v2 shipped.** `StreamError::Backpressure` now fires on both concurrent-caller races (v1) and network-speed overruns (v2: receiver-driven byte credit windows via `SUBPROTOCOL_STREAM_WINDOW = 0x0B00`). Same error variant, same SDK helpers, same daemon patterns. The admission unit is bytes, not packets; `StreamConfig::window_bytes` is now the initial credit window and defaults to 64 KB. Design details: [`STREAM_BACKPRESSURE_PLAN_V2.md`](STREAM_BACKPRESSURE_PLAN_V2.md).

---

*Original v1 design preserved below.*

`StreamError::Backpressure` is defined as a variant today but never returned; `send_on_stream` either succeeds or surfaces socket-level failures as `StreamError::Transport`. The daemon layer has no first-class way to ask "is this stream under pressure?" without parsing transport errors.

This plan ships a real Backpressure signal in v1 without wire changes or per-peer credit accounting, and gives daemons a small set of reusable patterns (drop / retry / app-buffer) in the SDKs. v2 is a forward-compatible swap to round-trip credit windows where daemon code does not change — only the internal condition that triggers Backpressure does.

## What already works

- **`StreamError::Backpressure` variant** in `stream.rs` — already part of the public API, just never constructed.
- **Per-stream fair scheduling** — `FairScheduler::enqueue` in `router.rs` already returns `bool`, surfacing queue-full on the forward path; the scheduler is the reference shape for what we want on the local-send path too.
- **`StreamState` per session** in `session.rs` — the natural home for a per-stream TX counter.
- **`StreamStats` accessor** — already public; straightforward to add a `backpressure_events` counter.
- **Per-stream `max_queue_depth`** config in `MeshNodeConfig` and `RouterConfig` — reusable as the v1 window default.

## Gaps this plan fills

1. **`send_on_stream` bypasses every queue**. It builds a packet and awaits the socket directly. There is no local queue to fill, so there is no "queue-full" condition to surface. The OS send buffer absorbs bursts silently until it backs up, at which point the error comes through as `io::Error` and we wrap it as `Transport` — indistinguishable from a hard socket failure.

2. **No per-stream accounting for in-flight local sends**. Two daemons sending on the same stream can jointly flood the socket and neither sees `Backpressure`.

3. **No SDK ergonomics for the common patterns**. Daemons that want drop-on-pressure, retry-with-backoff, or app-level buffering have to re-implement the match arm every time.

4. **Stats don't distinguish pressure from transport failure**. Anything that wasn't `Ok(())` is lumped into `errors` — callers can't tell "I'm going too fast" from "my peer died."

## Goals (v1)

- `Stream::send` / `MeshNode::send_on_stream` returns `StreamError::Backpressure` when a per-stream in-flight counter would exceed its configured cap.
- `StreamStats.backpressure_events: u64` exposed as a new field.
- Rust/TS/Python SDKs ship `send_with_retry(events, max_retries)` and `send_blocking(events)` helpers that implement the "exponential backoff over `Backpressure`" pattern once, so daemons don't hand-roll it.
- Docs land the three canonical daemon patterns (drop, retry, app-buffer) in `TRANSPORT.md`.

## Non-goals

- **Wire-format changes.** No new control messages in v1. The window is purely local.
- **Per-peer credit accounting.** v1 catches concurrent local-caller flooding, not slow receivers. A real credit window is v2 (see below).
- **Auto-throttling.** `Backpressure` is a *signal*, not a policy. The daemon decides what to do. The transport never sleeps, retries, or buffers on its own behalf.
- **Blocking `Stream::send`.** `send` is a non-blocking async call that either enqueues or returns `Backpressure`. "Block on pressure" is one specific policy (`send_blocking`) layered on top.
- **Receive-side flow control.** v1 is outbound only. A misbehaving sender is handled by the receiver's own ingestion queue (separate concern).
- **Renaming `window_bytes`.** The existing field stays; v1 interprets it as "max in-flight packets" (packets, not bytes) and v2 will switch the interpretation to bytes without changing the name. Documented in `stream.rs`.

## Design

### Option choice — counter on `StreamState` (Option A, chosen)

Two viable v1 shapes, both local-only, both honoring the non-goals:

- **Option A (chosen): a per-stream in-flight counter on `StreamState`.** `send_on_stream` reads the counter, returns `Backpressure` on over-budget, otherwise increments, sends, decrements. Minimal state, no coupling to the forwarding scheduler.
- **Option B: route `send_on_stream` through `FairScheduler::enqueue` and surface its `bool` return as `Backpressure`.** Zero new state; one line of plumbing. But local outbound sends now depend on scheduler internals, the forwarding path's queue, and whatever drainer we wire to pull from it — and when v2 swaps local backpressure to bytes-in-flight + credit windows, we'd have to decouple again.

Option A keeps `send_on_stream` synchronous-await-socket, the scheduler focused on forwarding, and the v2 swap local to `StreamState`. Option B buys us one fewer field in exchange for coupling that we'd later have to unwind. We pick A.

### v1 — per-stream in-flight counter on `StreamState`

Add two fields to `StreamState`:

```rust
/// Max outstanding packets on this stream before `send` returns
/// Backpressure. Taken from `StreamConfig.window_bytes` at open time
/// (0 = inherit `MeshNodeConfig.max_queue_depth`).
tx_window: u32,

/// Current in-flight packets on this stream — incremented before the
/// socket send and decremented after completion (success or failure).
tx_inflight: AtomicU32,
```

`MeshNode::send_on_stream` gains a check before each socket send.
Two correctness properties the check must preserve:

1. **Atomic check + increment.** A naive `load(Acquire); if cur < window { fetch_add(1) }` race-oversubscribes the window: two threads both read `cur == window-1`, both increment, and the counter ends up at `window+1`. Use a CAS loop so the admission decision and the increment happen together:

    ```rust
    loop {
        let cur = state.tx_inflight.load(Acquire);
        if cur >= state.tx_window {
            state.backpressure_events.fetch_add(1, Relaxed);
            return Err(StreamError::Backpressure);
        }
        if state
            .tx_inflight
            .compare_exchange_weak(cur, cur + 1, AcqRel, Acquire)
            .is_ok()
        {
            break;
        }
        // CAS lost the race — retry with the fresh value.
    }
    ```

2. **Release on every exit path, including async cancellation.** A plain "increment, await, decrement" shape leaks the slot when the caller drops the future mid-`await` (e.g., `tokio::select!` races the send against a shutdown signal). The cure is a RAII guard that decrements on `Drop`:

    ```rust
    struct TxSlotGuard {
        session: Arc<NetSession>,
        stream_id: u64,
    }
    impl Drop for TxSlotGuard {
        fn drop(&mut self) {
            if let Some(state) = self.session.try_stream(self.stream_id) {
                state.release_tx_slot();
            }
        }
    }
    // ...
    let guard = acquire_slot(&session, stream_id)?; // CAS-admit inside
    let result = socket.send_to(&packet, peer_addr).await;
    drop(guard); // explicit for clarity; also fires on cancellation/panic
    result.map_err(...)?;
    ```

   The guard looks up the stream fresh at drop time rather than holding a `DashMap` ref across the `await` — holding a shard lock across await would risk a deadlock with the receive loop.

The counter is incremented **per packet**, not per `send()` call; large batches that straddle `MAX_PAYLOAD_SIZE` split into multiple packets and consume multiple window slots.

### What v1 actually catches

v1 is a **concurrent-caller** guard, not a network-speed guard:

- Two tasks sending on the same `Stream` simultaneously compete for the same window. One sees `Backpressure`.
- A single serial caller that does `for e in stream: await stream.send(&e)` stays inside the window (always 0 → 1 → 0) and never sees `Backpressure`. It gets implicit socket-level backpressure as `Transport(io::Error)` instead, unchanged from today.

The v2 credit-window swap is what extends coverage to network-speed cases. v1's narrower scope is intentional: it gives daemons a correct variant to pattern-match on, with an implementation that's small enough to ship and hard to get wrong.

### Stats

Extend `StreamStats`:

```rust
pub struct StreamStats {
    // ...existing fields...
    /// Count of `Stream::send` calls that returned `Backpressure`
    /// since the stream was opened.
    pub backpressure_events: u64,
}
```

Plumbed from a `backpressure_events: AtomicU64` on `StreamState`, incremented in the Backpressure branch of `send_on_stream`.

### Daemon patterns (documentation, not code)

Three patterns land in `TRANSPORT.md` as the reference for daemon authors:

**Drop (telemetry / sampled streams):**
```rust
match mesh.send_on_stream(&s, &[e]).await {
    Ok(()) => {}
    Err(StreamError::Backpressure) => metrics.inc("dropped_under_pressure"),
    Err(e) => tracing::warn!(error = %e, "send failed"),
}
```

**Retry with exponential backoff (control / important events):**
```rust
mesh.send_with_retry(&s, &[e], 8).await?;
```

**App-level buffer (daemon-local queue, background drainer):**
```rust
// Daemon keeps its own bounded VecDeque; drains to stream as pressure lifts.
// `Stream` provides the signal; the app-level queue provides the policy.
```

### SDK helpers

Rust (Net core):

```rust
impl MeshNode {
    /// Send `events` on `stream`, retrying on `Backpressure` with
    /// exponential backoff up to `max_retries`. Transport failures are
    /// returned immediately (not retried — they're not a pressure
    /// signal, they're a real error).
    pub async fn send_with_retry(
        &self,
        stream: &Stream,
        events: &[Bytes],
        max_retries: usize,
    ) -> Result<(), StreamError>;

    /// Convenience: `send_with_retry(..., usize::MAX)` with a generous
    /// default backoff cap. Blocks until delivery or transport error.
    pub async fn send_blocking(
        &self,
        stream: &Stream,
        events: &[Bytes],
    ) -> Result<(), StreamError>;
}
```

Backoff schedule: starts at 5 ms, doubles, caps at 200 ms. Total max wait = O(max_retries × 200 ms).

TypeScript / Python SDK: equivalent helpers; `BackpressureError` surfaces as a concrete JS `class` / Python `Exception` subclass so `instanceof` / `isinstance` works at the daemon layer.

### v2 sketch — real credit windows (out of scope here, referenced only for forward-compat)

When we add proper credit accounting:

- `tx_inflight` measured in **bytes** instead of packets.
- New control message `StreamWindow { stream_id, credit }` — receiver grants credit out-of-band.
- `tx_window` starts at a small default; grows as credit arrives; shrinks as bytes go out.
- `tx_window` decrements on ack / receiver-driven acknowledgment (for `Reliable`) or on send for `FireAndForget` with a configured recovery timer.

Daemon code does not change. The Backpressure variant keeps its meaning; only the condition that triggers it moves from "local counter full" to "no credit from peer."

## Implementation steps

1. **Step 1** — `StreamState` fields (`tx_window`, `tx_inflight`, `backpressure_events`) + accessors. Config plumbing from `StreamConfig.window_bytes` (default fallback to `MeshNodeConfig.max_queue_depth`).
2. **Step 2** — `send_on_stream` check + increment/decrement around each socket send. `StreamError::Backpressure` returned on over-budget.
3. **Step 3** — `StreamStats.backpressure_events` field; populated from `StreamState` atomic.
4. **Step 4** — `MeshNode::send_with_retry` and `send_blocking` helpers with the documented backoff schedule.
5. **Step 5** — TS + Python SDK: `BackpressureError` class, wrapper helpers with the same retry/blocking API.
6. **Step 6** — `TRANSPORT.md` "Back-pressure" section: the three daemon patterns + examples in Rust/TS/Python.
7. **Step 7** — Tests.

## Tests

**Unit (`session.rs`):**
- `test_stream_state_tx_window_trips_backpressure` — fill window, next acquire returns an error; counter increments.
- `test_stream_state_tx_window_releases_on_send_completion` — after drain, send succeeds again.

**Unit (`mesh.rs` / integration via `send_on_stream`):**
- `test_send_on_stream_backpressure_when_concurrent` — spawn N concurrent senders on a window-1 stream; exactly one succeeds per window slot, the rest see `Backpressure`.
- `test_send_on_stream_stats_backpressure_counter_increments` — verify `stream_stats().backpressure_events` reflects rejections.

**SDK:**
- `test_send_with_retry_eventually_succeeds` — window-1 stream; a retry loop drains the pressure and the second call succeeds.
- `test_send_with_retry_surfaces_transport_immediately` — simulate a transport error; helper must NOT retry, must return the error.

## Risks and open questions

- **Serial-caller blind spot.** A single serial sender on a stream won't see Backpressure in v1; the counter never exceeds 1. Documented as expected; v2 fixes it. Daemons that care about rate-limiting a single sender should use app-level pacing (or wait for v2).
- **Window units.** v1 counts packets; v2 will count bytes. Keeping the field name `window_bytes` is intentional — it becomes accurate in v2 — but v1 comments must explicitly call this out to avoid surprise.
- **Helper backoff schedule.** 5 ms → 200 ms exponential is a reasonable default; may need knobs later. Start without; add if callers request.
- **Stats granularity.** `backpressure_events` counts *events*, not *packets dropped* (a batch that straddles multiple packets and is rejected at the first slot counts once, not per-packet). Documented; add a second counter if product demand shows up.
- **No queue between `send_on_stream` and the socket.** v1 keeps the send path synchronous-await. If v2 adds a scheduler hop, packets queued at the moment pressure spikes get absorbed instead of rejected — but v2 also gives us wire-level credit, so the resulting Backpressure latency is a more meaningful signal. For v1, the synchronous path is the right simplicity.

## Summary

v1 is a **local-counter** Backpressure signal with no wire changes — enough to make the variant real, enough to make SDK retry helpers useful, and small enough that the v2 credit-window swap doesn't change daemon code. The hard part (coordinating credit across peers) is deferred; the easy part (giving daemons a first-class pressure signal and documenting the three handling patterns) ships now.
