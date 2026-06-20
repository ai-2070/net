# Observability — Stats, Tuning, Wiring to Metrics

Under the default `DropOldest` (and the alternative `DropNewest`) backpressure modes, the bus loses events **silently** — `emit()` returns success even when the event was evicted. Without instrumentation you have no signal that data is being lost. This file lists the stats surfaces every SDK exposes, what each counter means, the tuning knobs, and how to wire them into Prometheus / OpenTelemetry.

If your service ships to production on the default config without scraping `events_dropped`, you will not learn about your first outage from the bus.

---

## Bus-level stats

`node.stats()` / `bus.stats()` returns a flat snapshot of atomic counters. Cheap to call (handful of relaxed loads); safe to scrape on a 1–10 s timer.

### Rust (`net_sdk::Stats`)

Source: `net/crates/net/sdk/src/net.rs:26-33` (the SDK `Stats` struct) and `net/crates/net/src/bus.rs:149-172` (the underlying `EventBusStats`).

| Field | Type | Meaning |
|---|---|---|
| `events_ingested` | `u64` | Total events the bus accepted into a shard's ring buffer. |
| `events_dropped` | `u64` | Events the bus rejected — either evicted by `DropOldest`, dropped at the boundary by `DropNewest`, surfaced as `Ingestion` errors under `FailProducer`, or sampled away under `Sample(N)`. Includes events that timed out at the Phase 3 adapter-flush barrier. |
| `batches_dispatched` | `u64` | Batches successfully handed to the adapter. |

The underlying `EventBusStats` (visible via `node.bus().stats()`) additionally exposes `events_dispatched: AtomicU64` — total events that made it through to the adapter (sum across batches). The Phase 2 flush invariant is stated against this counter:

> By the time `flush()` returns, `events_dispatched + events_dropped >= events_ingested_at_flush_entry` (`net/crates/net/src/bus.rs:168-171`).

Every event ingested before the flush call is accounted for in exactly one bucket — dispatched or dropped.

### TypeScript (`Stats`)

Source: `net/crates/net/bindings/node/index.d.ts:1890-1895`.

| Field | Type | Meaning |
|---|---|---|
| `eventsIngested` | `bigint` | Same as Rust `events_ingested`. |
| `eventsDropped` | `bigint` | Same as Rust `events_dropped`. |

The TS binding **does not surface `batches_dispatched`** at the `Stats` shape. If you need it, drop into the Rust core via a custom binding. The values are `bigint` not `number` because production busses outrun `i64::MAX` on a multi-month uptime — don't `Number(s.eventsIngested)` blindly.

### Python (`net.Stats`)

Source: `net/crates/net/bindings/python/src/lib.rs:159-172` and `net/crates/net/bindings/python/python/net/_net.pyi:66-75`.

| Field | Type | Meaning |
|---|---|---|
| `events_ingested` | `int` | Same as Rust. |
| `events_dropped` | `int` | Same as Rust. |

Python also does not surface `batches_dispatched`. Python int handles `u64` natively.

### Go / C (`net_stats_t`)

Source: `net/crates/net/include/net.h:92-97`. Exposed via `net_stats(handle, buf, len)` (JSON form) and `net_stats_ex(handle, *out)` (struct form).

| Field | Type | Meaning |
|---|---|---|
| `events_ingested` | `uint64_t` | Same as Rust. |
| `events_dropped` | `uint64_t` | Same as Rust. |
| `batches_dispatched` | `uint64_t` | Same as Rust. |

The Go binding mirrors `net_stats_ex` directly — same field names.

### Cross-SDK naming

Snake-case everywhere except TS, which is camel-case (`eventsIngested` / `eventsDropped`). Field set is **not** uniform: Rust, Go, and C have all three counters; TS and Python expose only ingested + dropped.

---

## The silent-drop trap

Under `DropOldest` (default) and `DropNewest`, **nothing surfaces at the call site when an event is dropped.** `emit()` returns a `Receipt`. `publish()` returns `true`. The drop is visible only in `events_dropped`.

Always alert on:

- **Rate** — `rate(events_dropped[1m]) > 0` sustained for >5 minutes.
- **Ratio** — `events_dropped / (events_ingested + events_dropped)`. >1% sustained means something is genuinely wrong (consumer can't keep up, transport is slow, shards are too few). Spikes during scale events are normal — alert on the sustained baseline, not the peaks.

If the workload cannot tolerate silent drops at all, switch to `Backpressure::FailProducer` so the producer learns synchronously. That trades silent loss for a noisy error path — pick deliberately.

---

## Mesh / transport stats

When running the mesh transport, these surface alongside the bus stats. They live on `MeshNode`, not on `NetNode`.

### Per-stream stats — `MeshNode::stream_stats(peer, stream_id)` and `all_stream_stats(peer)`

Source: `net/crates/net/src/adapter/net/stream.rs:164-198`.

| Field | Type | Meaning |
|---|---|---|
| `tx_seq` | `u64` | Next TX sequence number. Effectively "packets enqueued since open". |
| `rx_seq` | `u64` | Highest RX sequence number observed. |
| `inbound_pending` | `u64` | Events buffered on the inbound queue waiting for the caller to poll. |
| `last_activity_ns` | `u64` | Unix-nanos of last in/out activity. Idle-eviction signal. |
| `active` | `bool` | Whether the stream is open. |
| `backpressure_events` | `u64` | Cumulative `send` calls that returned `BackpressureError` because credit ran out. Monotonic until close+reopen. |
| `tx_credit_remaining` | `u32` | Bytes of send credit remaining. `0` means the next send is rejected as Backpressure. |
| `tx_window` | `u32` | Configured initial credit window (bytes). `0` disables backpressure on this stream. |
| `credit_grants_received` | `u64` | Cumulative `StreamWindow` grants from the peer (sender side). |
| `credit_grants_sent` | `u64` | Cumulative `StreamWindow` grants emitted to the peer (receiver side). |

TS / Python expose the same shape on `NetStreamStats` (`net/crates/net/bindings/node/index.d.ts:1667-1678`, `net/crates/net/bindings/python/src/lib.rs:1000-1021`); TS uses camelCase (`txSeq`, `lastActivityNs`, `backpressureEvents`, `txCreditRemaining`, …). `last_activity_ns` is a `bigint` in TS — well above `2^53`, will quietly truncate if you cast.

### Retransmit-window evictions — `untracked_evictions()`

The reliable-stream retransmit window has a bounded number of slots. If the tx-credit window ever admits more in-flight packets than the retransmit window can track, the oldest unacked descriptor is evicted — and a packet lost *after* eviction can never be retransmitted. That is **silent loss**, the stream-layer cousin of `events_dropped`. The retransmit window is sized from the tx-window so the invariant `tx-window ≤ retransmit-window` holds by default, and any eviction that still occurs is surfaced:

- **`ReliableStream::untracked_evictions() -> u64`** — cumulative count of evicted, no-longer-trackable descriptors. Should be `0` in a correctly-sized deployment; a non-zero and climbing value means a window misconfiguration is silently dropping retransmittable data.
- A **rate-limited `warn!`** fires on the first eviction and every 64th after, so the loss is visible in logs even without scraping.

**Alert on `untracked_evictions > 0`** the same way you alert on `events_dropped` — it is the reliable-stream silent-loss signal, and under default backpressure it is otherwise invisible. Source: `net/crates/net/src/adapter/net/reliability.rs:445`.

### NAT traversal — `mesh.traversal_stats() -> TraversalStatsSnapshot`

Source: `net/crates/net/src/adapter/net/traversal/mod.rs:108-152`. Returned snapshot is `Copy`.

| Field | Type | Meaning |
|---|---|---|
| `punches_attempted` | `u64` | Punches the matrix mediated through the coordinator. Pre-wire fast-fails do not contribute. |
| `punches_succeeded` | `u64` | Attempts that produced a direct (non-relayed) session. |
| `relay_fallbacks` | `u64` | `connect_direct` calls that ended on the routed-handshake path — only counted after the routed path actually succeeded. |
| `port_mapping_active` | `bool` | A UPnP/PCP port mapping is currently installed and being advertised. |
| `port_mapping_external` | `Option<SocketAddr>` | The mapped external address while active. |
| `port_mapping_renewals` | `u64` | Successful renewals since the current install. Resets on fresh install. |

Useful derived metrics:

- **Effectiveness** = `punches_succeeded / punches_attempted`
- **Fallback rate** = `relay_fallbacks / (punches_succeeded + relay_fallbacks)`

A non-zero fallback rate is **not a problem**. It means the routed-handshake path won — the connection still works, just one extra hop. Alert only if fallback rate is climbing while effectiveness is dropping (worsening NAT environment).

### Failure detector / route table

Source: `net/crates/net/src/adapter/net/failure.rs` and `route.rs`.

- `FailureStats` (`failure.rs:114-127`) — `nodes_tracked`, `nodes_healthy`, `nodes_suspected`, `nodes_failed`, `total_failures`, `total_recoveries`. Read peer health here.
- `RecoveryStats` (`failure.rs:730-741`) — `reroutes`, `retries`, `dropped`, `queued`, `avg_recovery_ms`.
- `AggregateStats` (`route.rs:716-727`) — `routes`, `streams`, `packets_in`, `packets_out`, `packets_dropped`. Route-table size and aggregate forwarding counters.

These are not all wired to every binding — for cross-SDK consumption, scrape via the FFI helpers (`net_mesh_traversal_stats` is the only one with a stable C entry today; for the rest, read via the Rust handle).

---

## Adapter-specific stats

**Redis adapter.** Subscriber-side lag and the dedup helper are the two surfaces. The dedup `RedisStreamDedup.len()` / `capacity()` (Python) tells you how full the consumer-side ID set is — when `len() / capacity()` approaches 1, dedups will start rolling off and you may re-deliver. Adapter source: `net/crates/net/src/adapter/redis/`.

**JetStream adapter.** Stream-age, consumer-pending, and redelivery counters surface from JetStream itself — the adapter does not duplicate them. Read from your NATS deployment's monitoring port (`/jsz`) and correlate with `events_dispatched` on the publisher. Adapter source: `net/crates/net/src/adapter/jetstream/`.

---

## Wiring to Prometheus

The bus stats are atomic counters; the simplest path is a periodic scrape from your handler that updates a `prometheus`-crate `IntGaugeVec` (or `IntCounterVec` if you reset between scrapes — gauges are simpler).

### Rust

```rust
use prometheus::{IntGauge, register_int_gauge};

let ingested = register_int_gauge!("net_events_ingested", "Total ingested").unwrap();
let dropped  = register_int_gauge!("net_events_dropped",  "Total dropped").unwrap();

tokio::spawn({
    let node = node.clone();
    async move {
        let mut t = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            t.tick().await;
            let s = node.stats();
            ingested.set(s.events_ingested as i64);
            dropped.set(s.events_dropped as i64);
        }
    }
});
```

The bus also emits structured `tracing` events (the bus loop logs at INFO/WARN with fields `events_ingested`, `events_dropped`, etc. — `net/crates/net/src/bus.rs:1294-1296`). Wire `tracing-subscriber` if you want them in your log pipeline. See `runtime.md` for the lifecycle context.

### TypeScript

The napi binding emits no metrics. Scrape `node.stats()` from a `setInterval` in your handler and expose via `prom-client`. Cast `bigint` carefully — for values that fit, `Number(s.eventsIngested)` is fine; for the long-running case, keep them as `bigint` and only convert to `number` when populating the gauge.

### Python

Same shape — `threading.Timer` or an `asyncio` task that calls `node.stats()` every few seconds and updates `prometheus_client.Gauge`. The native module already releases the GIL on the `stats()` call.

---

## Wiring to OpenTelemetry

Same model as Prometheus: the bus stats are aggregates, not events. Map them to OTel `Counter` / `UpDownCounter` / `Gauge` instruments via a periodic scrape callback (OTel's "observable" instrument pattern fits exactly).

```rust
let meter = global::meter("net");
let _ = meter
    .u64_observable_counter("net.events.ingested")
    .with_callback(move |obs| obs.observe(node.stats().events_ingested, &[]))
    .init();
```

Do not try to map each bus event to an OTel span — they are aggregate counters, not per-event records, and the volumes will destroy your tracing backend.

---

## Tuning knobs

| Knob | Where | When to touch |
|---|---|---|
| `shards` | `Net::builder().shards(N)` (Rust) / `NetNode({ shards: N })` (TS) / `Net(num_shards=N)` (Py/Go/C) | Default = `available_parallelism()`. Increase only if a single shard's drain worker is CPU-saturated. |
| `ring_buffer_capacity` | builder / config (`net/crates/net/src/config.rs:21`) | Default `1 << 20` per shard. Increase if `events_dropped` spikes on bursty traffic. Memory cost = `shards × capacity × avg_event_size`. |
| `batch.max_delay` | `BatchConfig` (`net/crates/net/src/config.rs:301,317`) | Default `10 ms`. Lower for latency, higher for throughput. Presets: `low_latency` = 1 ms, `high_throughput` = 5 ms. |
| `backpressure` | `Backpressure::{DropOldest,DropNewest,FailProducer,Sample(N)}` (`net/crates/net/sdk/src/config.rs:12-29`) | `DropOldest` (default) = newest-wins. `DropNewest` = oldest-wins. `FailProducer` surfaces a real error. `Sample(N)` keeps 1-in-N when overloaded. |
| `adapter_timeout` | builder (`net/crates/net/src/config.rs:39,73`) | Phase 3 cap on `adapter.flush()`. Default `30 s`. Lower if your adapter is local + cheap; events that miss this window count toward `events_dropped`. |
| `scaling` (`ScalingPolicy`) | builder (`net/crates/net/src/config.rs:672`) | Off by default. Auto-scales shards on fill ratio + velocity. Turn on when bursty workloads make fixed provisioning waste RAM. |
| mesh `heartbeat_interval` | `NetAdapterConfig::with_heartbeat_interval` (`net/crates/net/src/adapter/net/config.rs:64,184`) | Default `5 s`. Lower for faster failure detection (more chatter). |
| mesh `session_timeout` | `NetAdapterConfig::with_session_timeout` (`config.rs:66,190`) | Default `30 s`. **Must be > heartbeat_interval** (validated at build, `config.rs:246`). Failure-detector timeout. |

---

## Phase 2 flush invariant

`flush()` is a delivery barrier. It blocks until `events_dispatched + events_dropped >= events_ingested_at_flush_entry` (`net/crates/net/src/bus.rs:859-905`). Every event ingested before the call entered is, by the time `flush` returns, accounted for in exactly one bucket — dispatched to the adapter or counted as a drop. Use `flush()` before `shutdown()` if you cannot tolerate the in-flight loss documented in `runtime.md` § "The shutdown contract".

Default shutdown budget under default config: Phase 1 (~5 s ring drain) + Phase 2 (~2 s flush wait) + Phase 3 (`adapter_timeout`, default 30 s). If you've lowered `adapter_timeout`, the total shutdown deadline shrinks accordingly.

---

## Per-feature observability pointers

`tracing` targets — set per-target log levels via `RUST_LOG` or `tracing-subscriber` directives.

| Feature | Target |
|---|---|
| Mesh handshake / session | `net::adapter::net::mesh` |
| Routing decisions | `net::adapter::net::router` |
| NAT traversal | `net::adapter::net::traversal` |
| Failure detector | `net::adapter::net::failure` |
| Redis adapter | `net::adapter::redis` |
| JetStream adapter | `net::adapter::jetstream` |

Set `RUST_LOG=net::adapter::net::traversal=debug,net::adapter::net::failure=info` to drill into a specific subsystem without flooding the rest.

---

## Cross-references

- `runtime.md` § "The shutdown contract" — when each stat is valid. Reading `events_ingested` after `shutdown` is fine (the counters are not torn down). Reading mesh stream/traversal stats after the mesh socket is closed is not — `stream_stats` returns `None` once the session is gone.
- `runtime.md` § "Debugging: 'Why are my events missing?'" — checklist that uses these counters as the diagnostic surface.
- `concepts.md` § "Backpressure: silence, not a signal" — the design rationale for why drops are silent by default.
