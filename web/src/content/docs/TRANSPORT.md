# Transport Layer

The foundational layer of the Net mesh. Encrypted UDP with zero-allocation hot paths, multi-hop forwarding, adaptive batching, fair scheduling, failure detection, and swarm discovery.

## Wire Format

Every Net packet starts with a 64-byte header aligned to a single CPU cache line. Forwarding nodes read one cache line, make a routing decision, and forward without decrypting the payload.

```
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|         MAGIC (0x4E45)        |     VER       |     FLAGS     |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|   PRIORITY    |    HOP_TTL    |   HOP_COUNT   |  FRAG_FLAGS   |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|       SUBPROTOCOL_ID          |        CHANNEL_HASH           |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                         NONCE (12 bytes)                      |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                       SESSION_ID (8 bytes)                    |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                       STREAM_ID (8 bytes)                     |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                       SEQUENCE (8 bytes)                      |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|      SUBNET_ID (4 bytes)      |     ORIGIN_HASH (4 bytes)     |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|       FRAGMENT_ID             |        FRAGMENT_OFFSET        |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|       PAYLOAD_LEN             |        EVENT_COUNT            |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

**Constants:**
- Magic: `0x4E45` (ASCII "NE")
- Version: 1
- Max packet: 8,192 bytes
- Max payload: 8,096 bytes (packet - header - Poly1305 tag)
- Nonce: 12 bytes (counter-based)
- Tag: 16 bytes (Poly1305)

**Packet flags:** `RELIABLE`, `NACK`, `PRIORITY`, `FIN`, `HANDSHAKE`, `HEARTBEAT`

## Encryption

**Handshake:** Noise NKpsk0 pattern via the `snow` crate. The initiator is anonymous, the responder's static key is known in advance. A pre-shared key adds symmetric authentication. Direct UDP is the default path (`MeshNode::connect`), but when two peers have no direct path, `MeshNode::connect_via(relay_addr, …)` carries the Noise messages inside `SUBPROTOCOL_HANDSHAKE` (0x0601) over an existing encrypted session through a relay — the relay sees authenticated Noise bytes but cannot forge them or derive the post-handshake session keys. See [`HANDSHAKE_RELAY_PLAN.md`](HANDSHAKE_RELAY_PLAN.md) for the design.

**Payload encryption:** ChaCha20-Poly1305 AEAD with counter-based nonces. Each session derives separate TX/RX `SessionKeys` from the Noise handshake. The header is never encrypted -- only the payload.

```rust
pub struct SessionKeys {
    pub tx_key: [u8; 32],
    pub rx_key: [u8; 32],
    pub session_id: u64,
}
```

`PacketCipher` wraps the AEAD primitive with a monotonic counter for nonce generation, eliminating nonce-reuse risk without randomness.

## Packet Pools

Zero-allocation on the hot path. `PacketPool` pre-allocates reusable `BytesMut` buffers. `ThreadLocalPool` eliminates contention entirely -- each thread has its own pool.

```rust
PacketPool::new(capacity: usize)        // Shared pool (Arc<ArrayQueue>)
ThreadLocalPool::new(capacity: usize)   // Per-thread, zero contention
```

`PacketBuilder` constructs packets from pre-allocated buffers, batching multiple `EventFrame`s into a single packet. Events are length-prefixed (4-byte LE length + payload).

**Benchmark:** Thread-local pools achieve **23x contention advantage** over shared pools at 32 threads.

## Sessions

`NetSession` holds post-handshake state: TX/RX ciphers, per-stream sequence numbers, packet pool, and activity timestamps.

```rust
pub struct NetSession {
    session_id: u64,
    tx_cipher: Mutex<PacketCipher>,
    rx_cipher: Mutex<PacketCipher>,
    streams: DashMap<u64, StreamState>,
    pool: SharedPacketPool,
    origin_hash: u32,
    // ...
}
```

`SessionManager` validates session health and handles timeouts. Sessions are long-lived -- new sessions only form on handshake.

## Stream Routing & Fair Scheduling

`FairScheduler` provides round-robin fairness across streams. Each stream gets a configurable quantum of packets per round, multiplied by an opt-in per-stream `fairness_weight` (default 1). Priority streams can bypass the fairness queue.

```rust
pub struct RouterConfig {
    pub max_queue_depth: usize,   // Per-stream queue limit
    pub fair_quantum: usize,      // Base packets per stream per round
}
```

Stream IDs are opaque `u64` values. `stream_id_from_key(&str)` is the canonical helper for deterministic derivation from a name; callers are free to use anything.

## Streams (caller contract)

A stream is one logical channel within an encrypted session to a single peer. Multiple streams share the session's cipher and socket; they have independent sequence numbers, reliability state, and fair-scheduler weight.

**Opening and closing.**

```rust
let stream = mesh.open_stream(peer_node_id, stream_id, StreamConfig::new()
    .with_reliability(Reliability::Reliable)
    .with_fairness_weight(1)
    .with_close_behavior(CloseBehavior::DropAndClose))?;

mesh.send_on_stream(&stream, &events).await?;

mesh.close_stream(peer_node_id, stream_id);
```

- `open_stream` is **idempotent** for a given `(peer_node_id, stream_id)`. Re-opening returns a handle backed by the same underlying state; a config argument that differs from the first open is logged and ignored (first-open wins).
- `close_stream` drops the `StreamState` and stops inbound delivery for the stream. `CloseBehavior::DrainThenClose` is honored to the extent the scheduler has already flushed; there is no wire "drain" signal in v1.

**Lifecycle.**

- `StreamState` carries a `last_activity_ns` timestamp refreshed on every send and receive.
- The `MeshNode` heartbeat loop periodically evicts streams idle longer than `MeshNodeConfig::stream_idle_timeout` (default 5 min) and enforces the `max_streams` cap (default 4096) via LRU eviction, both logged (`reason=idle_timeout` or `reason=cap_exceeded`).

**Ordering contract.**

- `Reliability::Reliable` — FIFO delivery within the stream. Gaps trigger NACK-driven retransmission; the receive side reorders into sequence.
- `Reliability::FireAndForget` — best-effort. Sequence numbers are monotonic on the wire so callers who care can detect loss / reorder themselves, but the transport performs no recovery.
- **No ordering across streams.** A later-sent packet on stream A may arrive before an earlier-sent packet on stream B. Fair scheduling prevents starvation; cross-stream timing is unsynchronized.

**Stream IDs are opaque.** No range has reserved meaning at the transport layer. Subprotocol dispatch uses the `subprotocol_id` field in the header; do not conflate.

**Not multicast.** A stream is one flow to one peer. Sending the same payload to multiple peers is an application / daemon / channel-layer concern, not transport.

**Back-pressure.** `send_on_stream` returns `StreamError::Backpressure` when the stream's remaining send credit is below the payload size it wants to push. Credit is measured in **bytes**, seeded at open time from `StreamConfig::window_bytes` (default 64 KB; `0` disables backpressure entirely), decremented on each socket send, and replenished by receiver-driven `StreamWindow` grants (subprotocol `0x0B00`). The signal catches both concurrent callers racing on the same window AND a serial sender outrunning a slow receiver across the network — the latter no longer surfaces as `StreamError::Transport(String)` when the kernel buffer fills.

*Backpressure is a signal, not a policy.* The transport never retries, sleeps, or buffers on its own. Daemons pick one of three patterns per stream:

```rust
// 1. Drop on pressure — best for telemetry / sampled streams.
match mesh.send_on_stream(&stream, &[event]).await {
    Ok(()) => {}
    Err(StreamError::Backpressure) => metrics.inc("dropped_under_pressure"),
    Err(StreamError::Transport(e)) => tracing::warn!(error = %e, "send failed"),
    Err(StreamError::NotConnected) => {/* peer gone */}
}

// 2. Retry with backoff — best for important events.
mesh.send_with_retry(&stream, &[event], 8).await?;
// or: mesh.send_blocking(&stream, &[event]).await?;

// 3. App-level buffer — daemon-local VecDeque drained by a background
// task. Transport stays out of the policy; the app decides its own cap.
```

`send_with_retry(stream, events, max_retries)` and `send_blocking(stream, events)` apply a 5 ms → 200 ms exponential backoff to `Backpressure` only; `Transport` errors are returned immediately. `StreamStats` surfaces `backpressure_events`, `tx_credit_remaining`, `tx_window`, `credit_grants_received`, and `credit_grants_sent` for observability — a daemon author watching `tx_credit_remaining` approach zero with `backpressure_events` climbing can distinguish "local concurrent-caller pile-up" from "receiver grants exhausted."

**Fairness weight.** `StreamConfig::fairness_weight` is a quantum multiplier on the `FairScheduler`. It takes effect when a packet for this stream transits this node as a forwarder. Local outbound traffic currently bypasses the scheduler; the weight is still persisted so that a future refactor routing local outbound through the scheduler makes it load-bearing end-to-end without API churn.

**Statistics.** `mesh.stream_stats(peer, stream_id) -> Option<StreamStats>` and `mesh.all_stream_stats(peer) -> Vec<(u64, StreamStats)>` snapshot per-stream counters (tx/rx seq, inbound queue depth, last-activity timestamp, active flag).

## Multi-Hop Forwarding

`NetProxy` forwards packets without decrypting payloads. Reads the 64-byte header, decrements TTL, increments hop count, and forwards.

```rust
pub struct RoutingHeader {  // 16 bytes
    pub dest_id: u64,
    pub src_id: u64,
    // TTL, hop_count, flags packed in remaining bytes
}
```

`MultiHopPacketBuilder` constructs routed packets with layered routing headers. Per-hop latency tracking is optional.

**Benchmark:** 30.4 ns per hop (64B payload), 291 ns for a 5-hop chain.

## Routing

`send_routed(dest_id, batch)` consults `RoutingTable::lookup(dest_id)` to get the next-hop `SocketAddr`. The routing table is the **single source of truth** for "how do I reach X?" — `ProximityGraph` is an input (pingwaves feed into it) and a fallback (used by `ReroutePolicy` on failure when the table has no alternate). No two truths about routing.

**Pingwave-driven install.** When node X receives a pingwave originated by Y via direct peer Z, X calls `RoutingTable::add_route_with_metric(Y, next_hop=Z, metric=hop_count+2)`. The metric policy keeps the better (lower) entry, so direct routes (metric 1) always beat pingwave-installed routes. Routes age out via `RoutingTable::sweep_stale` on the heartbeat-loop tick; graph edges age out in lockstep via `ProximityGraph::sweep_stale_edges`.

**Three cheap loop-avoidance rules** (applied in `mesh.rs` on pingwave receipt):

1. **Origin self-check** — a pingwave with `origin_id == self_id` is dropped and installs no route. Defends against a peer echoing our own origin back at us, or a stale buffered pingwave replayed by a partitioned-then-healed peer.
2. **`MAX_HOPS` cap** — a pingwave with `hop_count >= 16` is dropped on receipt. TTL bounds forwarding at the emitter; `MAX_HOPS` is the receive-time counterpart that keeps an inflated-hop-count advertisement out of the routing table.
3. **Split horizon on re-broadcast** — before forwarding a pingwave to peer P, check `RoutingTable::lookup(origin)`. If the installed next-hop for `origin` is P's address, skip P. Prevents P from learning "we can reach origin in N+1 hops" and installing a backward loop.

**Metric.** Primary: `hop_count + 2`. Secondary tie-break: EWMA latency per `(origin, next_hop)` edge, fed by `now_us − pw.origin_timestamp_us` with `α = 1/8`. Clock-skew-sensitive, so advisory only; unreliable estimates degrade to "arbitrary equal-hop choice", which is acceptable.

**Reroute.** When the failure detector marks a peer failed, `ReroutePolicy::on_failure` walks the table's affected entries (entries whose `next_hop` matches the failed addr) and resolves a new next-hop in this order:

1. `RoutingTable::lookup_alternate(dest, exclude=failed_addr)` — returns the current entry if its next-hop isn't the excluded one. With the single-route-per-destination table this returns `None` whenever the affected entry *is* the failed-peer entry, which is the common case; the method is kept for clean API shape, not as a door to a deeper cache (see "Routing philosophy" below).
2. `ProximityGraph::path_to(dest)` — BFS over the topology graph. Returns the first hop of a path that isn't the failed node AND is a direct peer of ours.
3. Any direct peer that isn't the failed one — last-resort fallback. Best-effort; if it can't reach the destination, the failure detector will catch it on the next cycle.

The original `next_hop` is preserved in `saved_routes` so `on_recovery` can restore the pre-failure route when the failed peer comes back.

### Routing philosophy

Net's routing plane is deliberately minimal: pingwaves drive installation, the `RoutingTable` holds **one best route per destination**, and `ProximityGraph` is a helper that can *recompute* paths when needed — never a second source of truth for the fast path. This is a design choice, not an unfinished optimization.

**What this gives us.** Fast multi-hop routing with no separate control plane. Routing state that fits in a single `DashMap` entry per destination. Recomputation from pingwaves that completes in microseconds at our target scales. A fast path (`send_routed`) that only ever consults one data structure — no ranking, no cache-miss fallback, no stale-vs-fresh reconciliation.

**Why one route per destination, deliberately.** The tempting alternative is a ranked alternates list, or a full TCP-Cubic-style persistent path cache, or an IGP-style link-state database where every node holds the whole topology. We chose not to go there. The reasoning:

- In the **common case** (99% of the time), recomputing a path from fresh pingwaves + the local graph is so cheap that cached alternates save nanoseconds of decision time at the cost of real state. Not worth it.
- In the **catastrophic case** (the 1% that actually matter — a vehicle losing its primary compute, a site losing half its links in a storm, an RF environment going hostile), an entire class of previously-good routes can become wrong at once. A deep cache of alternates is now a liability: it surfaces **stale confidence** into the fast path, hides the fact that there is currently no safe route, and delays convergence while the cache ages out entry by entry.

Our bias: **"I don't know how to route this right now" is a better answer than "here's a route that was fine 5 seconds ago."** Recompute converges in a heartbeat interval; stale confidence can hide for as long as the TTL allows.

The failure mode this defends against is not "routing loops" or "black holes" specifically — those are bounded by TTL, `MAX_HOPS`, and split horizon regardless of table depth. The failure mode is **stale confidence**: the system holding a plausible-looking wrong answer for longer than convergence would have taken to produce a correct one.

**Graph as helper, not second truth.** `ProximityGraph` is an input to `RoutingTable` (pingwaves update both) and a fallback for `ReroutePolicy` (when the table has no usable alternate). It is never consulted on the fast path. There are not two sources of truth about routing — only one, with a derivation path that feeds it.

**What we are not building.** Persistent multi-route caches (TCP-Cubic-style), link-state databases (OSPF-style — every node holding the whole graph), path-vector attribute lists (BGP-style), or ECMP ranking tables. All of these trade recomputation cost for cached state. At Net's scale the trade doesn't pencil out, and the cache's behavior under fast-changing topology is exactly where these systems tend to ship bugs. A simple, recomputable routing plane is cheaper to reason about and safer under the failures we actually care about.

**Behavior under failure, summarized.**

- Next-hop peer dies → table entries through it are rerouted via the graph or marked unreachable. Fast-path callers get `Err` until convergence; `send_with_retry` / `send_blocking` absorb the gap.
- Half the mesh disappears → most cached routes are invalid anyway; pingwaves from the surviving subset rebuild a fresh picture within a few heartbeat intervals; the interim state is "no route," which is honest.
- Origin goes quiet → route for that origin ages out via `sweep_stale`; graph edges age out via `sweep_stale_edges` in lockstep. No separate invalidation message needed.

Predictable in the common case, safer in the catastrophic one.

## Reliability

Two modes implementing the `ReliabilityMode` trait:

| Mode | Overhead | Use case |
|------|----------|----------|
| `FireAndForget` | Zero | Sensor streams, telemetry |
| `ReliableStream` | Per-stream tracking | Commands, state updates |

`ReliableStream` uses selective NACKs: the receiver identifies missing sequence numbers and sends a `NackPayload` listing gaps. The sender retransmits only the missing packets. Timeout-driven retransmission handles lost NACKs.

## Adaptive Batching

`AdaptiveBatcher` dynamically sizes packet batches based on observed latency and queue depth.

- Target latency: 100 us (default)
- Batch range: 1 KB - 8 KB
- Burst detection: queue depth > 100 triggers larger batches
- EMA smoothing of batch latency for stable adaptation

**Benchmark:** +15-30% throughput for bursty workloads.

## Failure Detection

`FailureDetector` tracks node health via heartbeats.

```rust
pub enum NodeStatus {
    Healthy,
    Suspected,    // Missed heartbeats but not yet declared failed
    Failed,
    Unknown,
}
```

`RecoveryManager` handles route failover when nodes fail. `CircuitBreaker` prevents cascading failures by temporarily blocking traffic to failing nodes.

**Benchmark:** 32.4 ns per heartbeat processing, 362 ns for a full recovery cycle.

## Swarm Discovery

`Pingwave` is a lightweight neighbor discovery protocol. 24-byte packets flood the mesh with TTL-bounded propagation.

```rust
pub struct Pingwave {
    pub origin_id: u64,
    pub seq: u64,
    pub ttl: u8,
    pub hop_count: u8,
}
```

`CapabilityAd` announces what a node can do (GPU, tools, memory, model slots, tags). `LocalGraph` maintains a k-hop radius view of the mesh topology.

**Benchmark:** Graph construction for 5,000 nodes in 125 us.

## Socket Layer

`NetSocket` wraps Tokio UDP with optimized buffer sizes:

| Buffer | Default | Testing |
|--------|---------|---------|
| RX | 64 MB | 256 KB |
| TX | 64 MB | 256 KB |

On Linux, `BatchedPacketReceiver` uses `recvmmsg` to read up to 64 packets per syscall.

## Source Files

| File | Purpose |
|------|---------|
| `protocol.rs` | Wire format, header, EventFrame, NackPayload |
| `crypto.rs` | Noise handshake, ChaCha20-Poly1305, SessionKeys |
| `transport.rs` | UDP socket, PacketReceiver/Sender, buffer config |
| `session.rs` | NetSession, StreamState, SessionManager |
| `pool.rs` | PacketPool, PacketBuilder, ThreadLocalPool |
| `router.rs` | FairScheduler, stream routing, priority bypass |
| `route.rs` | RoutingTable, RoutingHeader, stream stats |
| `proxy.rs` | NetProxy, zero-copy forwarding, hop tracking |
| `batch.rs` | AdaptiveBatcher, latency-aware sizing |
| `reliability.rs` | FireAndForget, ReliableStream, selective NACKs |
| `failure.rs` | FailureDetector, RecoveryManager, CircuitBreaker |
| `swarm.rs` | Pingwave, CapabilityAd, LocalGraph |
| `linux.rs` | recvmmsg batch reads (Linux-only) |
| `config.rs` | NetAdapterConfig |
| `mod.rs` | NetAdapter, routing utilities |
