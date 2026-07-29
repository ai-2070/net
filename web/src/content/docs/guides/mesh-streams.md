---
title: Mesh Streams and Backpressure
description: Direct peer-to-peer streams outside the event bus, and the three canonical ways a daemon can respond when the window fills.
---

# Mesh Streams and Backpressure

The event bus fans events out to whoever is subscribed. Sometimes you want the
opposite: a direct, typed stream to **one specific peer**, with an explicit
reliability mode and a bounded in-flight window. That's a mesh stream.

## Opening one

```rust
use net_sdk::{MeshBuilder, Reliability, StreamConfig};

let node = MeshBuilder::new("127.0.0.1:9000", &[0x42u8; 32])?.build().await?;
// ... handshake with a peer ...

let stream = node.open_stream(
    peer_node_id,
    0x42,                                    // stream id
    StreamConfig::new()
        .with_reliability(Reliability::Reliable)
        .with_window_bytes(256),             // in-flight cap before backpressure
)?;
```

The window is the whole mechanism. Once that much data is in flight and
unacknowledged, sends fail with `Backpressure` until the peer catches up.

## Backpressure is a signal, not a policy

**The transport never retries or buffers on your behalf.** When the window is
full it tells you, immediately, and the decision is yours. That's deliberate —
a transport that silently buffered would turn a visible backpressure event into
invisible memory growth and unbounded latency.

Which means every stream producer has to pick one of three behaviours.

### 1. Drop — for telemetry and sampled streams

The right answer when a newer reading supersedes an older one.

```rust
match node.send_on_stream(&stream, &[payload]).await {
    Ok(()) => {}
    Err(SdkError::Backpressure) => metrics::inc("stream.backpressure_drops"),
    Err(SdkError::NotConnected) => { /* peer gone or stream closed */ }
    Err(e) => tracing::warn!(error = %e, "transport error"),
}
```

> `SdkError` is `#[non_exhaustive]`. Always include a wildcard arm — new
> variants are a minor-version change, and a closed match would stop compiling.
> Match the variants whose remediation differs; let the rest fall through.

### 2. Retry with backoff — for events that matter

```rust
node.send_with_retry(&stream, &[payload], 8).await?;   // 8 attempts
```

### 3. Block until the network lets up

```rust
node.send_blocking(&stream, &[payload]).await?;
```

Bounded, but generously: the worst case is roughly 13 minutes. Use it when
losing the event is worse than stalling the producer, and make sure stalling
the producer is actually acceptable — this is the option that turns a network
problem into an application-wide one if you pick it by default.

## Watching the window

```rust
let stats = node.stream_stats(peer_node_id, 0x42);   // None if closed/never opened
```

`StreamStats` carries per-stream tx/rx sequence numbers, in-flight bytes, the
window size, and `backpressure_events` — a cumulative count of rejections.
That counter is the one to alert on: a stream that is constantly backpressured
is a stream whose producer is outrunning its consumer, and no retry policy
fixes that.

## Other bindings

The same three patterns exist across the SDKs, with the naming each language
uses — `sendOnStream` / `sendWithRetry` in TypeScript, `send_on_stream` /
`send_with_retry` / `send_blocking` in Python. In every one, backpressure
surfaces as a typed error you branch on:
[Rust](/docs/sdk/rust/errors), [TypeScript](/docs/sdk/typescript/errors),
[Python](/docs/sdk/python/errors), [Go](/docs/sdk/go/errors).

## See also

- [Using the Event Bus](/docs/guides/event-bus) — the fan-out alternative
- [Submitted Is Not Completed](/docs/guides/submitted-is-not-completed)
- [Error Codes](/docs/reference/error-codes)
