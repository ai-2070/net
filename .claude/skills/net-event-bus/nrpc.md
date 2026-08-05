# nRPC — Request/Response Over the Mesh

Read when the user wants request/reply semantics: a typed call to a service, a returned value, deadlines, retries, hedging, response streaming. **The bus is broadcast pub/sub**; nRPC is a separate convention layer that sits on top of it for request/response.

If the user is doing pure pub/sub — fire-and-forget broadcast, hot subscribers, no return value — they don't want nRPC. Use the bus surface in `apis.md`. nRPC has a per-call cost (one extra subscription per (service, target) pair) that pub/sub doesn't.

---

## What nRPC is, in one sentence

nRPC turns a directed channel pair (`<service>.requests` / `<service>.replies.<caller_origin>`) into a typed RPC surface with deadlines, queue-group fan-out, response streaming, and end-to-end cancellation — riding on the same encrypted mesh + CortEX folds the bus already provides.

---

## Mental model

| Concept       | Meaning                                                                       |
| ------------- | ----------------------------------------------------------------------------- |
| Service name  | A string. The server publishes a capability `nrpc:<service>`; clients call by name OR address a specific node id. |
| `serve`       | Server-side: register a handler for `<service>`. Returns a `ServeHandle` that unregisters on close. In-flight handlers complete; no abort. |
| `call`        | Client-side: direct-addressed unary call against a specific node id.          |
| `call_service`| Client-side: service-discovery variant — picks any node advertising the service. |
| `call_streaming` | Server emits a stream of chunks; client consumes via async iterator. End-of-stream OR error frame terminates. |
| `ServeHandle` | Per-language: TS class with `.close()`, Python context manager (`with`), Rust `Drop`, Go `(*ServeHandle).Close()` + finalizer. |
| `RpcStream`   | Caller-side stream handle. `next()` blocks for next chunk; `close()` emits CANCEL; `grant(n)` issues explicit flow-control credit (no-op without window). |

**Wire shape:** every RPC is two events on the bus — a REQUEST on `<service>.requests` carrying `RpcRequestPayload { service, deadline_ns, flags, headers, body }` plus a per-caller `call_id` in the `EventMeta`, and a RESPONSE on `<service>.replies.<caller_origin>` correlated via the same `call_id`. Streaming RPCs emit multiple chunks plus a terminal end-or-error frame; flow-controlled streams add a GRANT subprotocol.

The reply-channel-per-caller convention keeps subscriptions cheap: a server holds one subscription per service name; a caller holds one subscription per `(service, target)` pair, lazily subscribed on first call and reused. CANCEL fires when the caller drops the future or `RpcStream` mid-stream.

---

## When to use nRPC vs the bus

| Need                                              | Surface     |
| ------------------------------------------------- | ----------- |
| Broadcast event to N subscribers, no reply        | **Bus**     |
| Hot subscriber that sees events emitted after join | **Bus**    |
| Typed call → typed response                       | **nRPC**    |
| Need a deadline + retry/hedge                     | **nRPC**    |
| Server emits a stream of chunks for one request   | **nRPC** (`call_streaming`) |
| Need to cancel mid-flight                         | **nRPC** (Drop / close)    |
| Persistence / replay                              | **Bus + RedEX/adapter**    |

If both ends are in your control AND you want a return value, pick nRPC. The bus has no return-value mechanism — folding it in via "two channels + correlation id" is exactly what nRPC does, except already implemented with deadlines, cancellation, and resilience helpers.

**If only certain organizations may call the service**, you want `serve_org` / `mesh.org(..).call(..)` rather than `serve_rpc` plus a hand-rolled gate — same nRPC transport, but with an offline-issued per-call admission proof and an announcement encrypted to an audience. Read `org.md` before writing that yourself. **And if the provider additionally sits inside a protected subnet** and must be reachable from outside its boundary, that is `serve_subnet_exported` / `org.call_exported` — still nRPC underneath, still org admission, plus a live gateway-authority check on the exported crossing. Read `subnet-auth.md`.

---

## Status codes + error model

`RpcStatus` is `u16`. Two stable application-status constants ship in every binding:

| Status hex | Constant                     | Trigger                                           |
| ---------- | ---------------------------- | ------------------------------------------------- |
| `0x0000`   | `RpcStatus::Ok`              | Normal response.                                  |
| `0x8000`   | `NRPC_TYPED_BAD_REQUEST`     | Typed handler couldn't decode the request body.   |
| `0x8001`   | `NRPC_TYPED_HANDLER_ERROR`   | Typed handler ran but returned an exception.      |

Protocol-defined band: `0x0000..=0x7FFF` (`Ok`, `Internal`, `Backpressure`, `Timeout`, `NotFound`, `BadRequest`, …). Application-defined band: `0x8000..=0xFFFF`. **If the user is defining their own status codes, they go in the application band**; protocol-defined slots are reserved.

Caller-side failures surface with a stable `nrpc:` prefix so cross-language code can pattern-match:

| Kind segment    | Typed class (per binding)            |
| --------------- | ------------------------------------ |
| `no_route`      | `RpcNoRouteError`                    |
| `timeout`       | `RpcTimeoutError`                    |
| `server_error`  | `RpcServerError` (carries `status`)  |
| `transport`     | `RpcTransportError`                  |
| `codec_encode`  | `RpcCodecError(direction='encode')`  |
| `codec_decode`  | `RpcCodecError(direction='decode')`  |
| `cancelled`     | `RpcCancelledError`                  |
| `capability_denied` | `RpcCapabilityDeniedError`       |
| anything else   | the base `RpcError` — the vocabulary is frozen, but forward-compatible |
| `breaker_open`  | `BreakerOpenError` — **not a wire kind**; a client-side resilience helper |

The eight wire kinds are single-sourced across Rust, Node, Python and Go; the
authoritative mapping is the comment block at `net/crates/net/bindings/node/errors.ts:55`.
An unrecognised kind degrades to the base class rather than throwing, so a new
kind added later does not break an existing catch site.

Each binding ships a `classifyError(e)` / `classify_error(e)` helper that maps a raw `nrpc:`-prefixed exception to the typed subclass. Used at catch sites where `instanceof` discrimination is awkward (e.g. fallback paths where the native module wasn't built; vitest dual-module-instance hazard).

---

## The four call shapes

One wire, one typed surface, four shapes. The skill's examples below are unary; the other three layer on the same primitive and ship across Rust, Node, Python (sync + async), and Go with the same wire contract.

| Shape | Serve | Call | When |
|---|---|---|---|
| **Unary** | `serve_rpc_typed` | `call_typed` / `call_service_typed` | one request, one reply. The default. |
| **Server-streaming** | `serve_rpc_streaming_typed` — handler gets a `ResponseSinkTyped<Resp>`, pushes chunks with `sink.send(&chunk)?`, returns `Ok(())` to close cleanly or `Err(msg)` to fail the stream | `call_streaming_typed::<Req, Resp>` → an async `Stream` | token streaming, download-with-progress, log tails. |
| **Client-streaming** | handler receives a `RequestStreamTyped<Req>` and returns one terminal `Resp` once the stream closes | `call_client_stream_typed::<Req, Resp>` → `call.send(&chunk).await?` × N, then `call.finish().await?` | uploads, batch submit. |
| **Duplex** | both directions stream independently | `call_duplex_typed::<Req, Resp>` → `call.into_split()` gives `(sink, stream)`; `sink.finish_sending().await` closes the request direction | interactive sessions, long-running coordination. |

A request chunk that fails to decode terminates a client-streaming/duplex stream with an `RpcError::Codec` item — it does not silently skip. `stream_window_initial` on `CallOptions` bounds the response direction; its request-direction mirror bounds the upload direction. Without a window, the server pumps as fast as the publish path takes it.

## Picking a target — `RoutingPolicy`

`call_typed` addresses one node id. `call_service_typed` consults the local capability index for nodes advertising `nrpc:<service>` and picks one per `opts.raw.routing_policy` (`src/adapter/net/mesh_rpc.rs`):

| Variant | Behavior |
|---|---|
| `RoundRobin` | **Default.** Round-robin via the per-`Mesh` `call_id` counter. Even distribution, load-blind. |
| `Random` | Stateless per-call random pick. |
| `Sticky { key: u64 }` | Consistent-hash to a target by `key` — same key hits the same node while the candidate set is stable. Session / shard / conversation affinity. |
| lowest-latency | Picks the smallest measured `latency_us` from the local `ProximityGraph`. Candidates with no proximity data sort to the bottom (better a known-fast node than a gamble), with a deterministic fallback to the first sorted candidate when nobody has data. |

If no node advertises the service, the call fails `RpcError::NoRoute` — which the default retry predicate deliberately does **not** retry.

## Capability-targeted calls — the `net-where:` predicate

You can require more than "any server for this service": ship a capability predicate alongside the call and let the *receiver* evaluate it against its own capability set. A mismatched receiver refuses without invoking the handler.

```rust
use net_sdk::capabilities::pred;
use net_sdk::mesh_rpc::{CallOptionsExt, CallOptionsTyped};

// `pred!` is a macro over dotted string keys — see `capabilities.md`.
let predicate = pred!(and [
    pred!(exists "hardware.gpu"),
    pred!(num_at_least "hardware.memory_gb", 16.0),
]);
let opts = CallOptionsTyped::default().with_where(&predicate)?;

let reply: InferenceReply =
    mesh.call_service_typed("inference.run", &args, opts).await?;
```

`with_where` encodes the predicate to JSON and pushes it into `CallOptions::request_headers` under `net-where`. Servers that opt into predicate-pushdown read it back via `RpcContextExt::where_predicate()`. A refusal surfaces as `RpcError::CapabilityDenied` (terminal — the default retry predicate skips it, because only a new announcement from the target can change the verdict); a call that matches no serving node is `RpcError::NoRoute`.

This is the "route this call to a node with these capabilities" primitive — no sidecar, no separate service-discovery layer, no second auth step. Predicate grammar is in `capabilities.md`.

## Cancellation

Cancellation is a substrate primitive, honored uniformly by all four call shapes. Reserve a token from the node handle, pair it with the call via `CallOptions::cancel_token`, and cancel from any thread:

```rust
let node = mesh.node_arc();
let token = node.reserve_cancel_token();

let opts = CallOptionsTyped {
    raw: CallOptions { cancel_token: Some(token), ..Default::default() },
    ..Default::default()
};
// … spawn the call with `opts` …
node.cancel(token);   // from anywhere → caller sees RpcError::Cancelled, CANCEL goes on the wire
```

**The race is handled for you.** A cancel that arrives in the gap between `reserve_cancel_token` and call construction is latched on an orphan entry; when the call registers it observes the flag and short-circuits to `RpcError::Cancelled` **without ever publishing the REQUEST**. Unused reservations age out on an orphan TTL.

Idiomatic per-binding wrappers all lower to the same token, so a TS client cancelling a Python server is wire-equivalent to any other pairing:

- **Node** — `AbortSignal`: pass `signal` in the call options, `signal.abort()`.
- **Python** — a cancel handle passed via `cancel=`, tripped with `.cancel()`.
- **Go** — `context.Context`: pass `ctx`, call `cancel()`.

`Cancelled` is terminal for retry purposes — you asked it to stop.

## Observers and per-service metrics

```rust
use net_sdk::mesh_rpc::{RpcCallEvent, RpcObserver};

struct MetricsSink;
impl RpcObserver for MetricsSink {
    fn on_call(&self, evt: RpcCallEvent) { record(&evt.method, evt.latency_ms, &evt.status); }
}
mesh.set_rpc_observer(Some(Arc::new(MetricsSink)));

let snap = mesh.rpc_metrics_snapshot();
for svc in &snap.services { println!("{}: {} calls", svc.service, svc.calls_total); }
```

`RpcCallEvent` carries the method name, caller/callee node ids, request/response byte counts, `latency_ms`, a `direction` (`Outbound` / `Inbound` — **v1 fires `Outbound` only**, so don't build a server-side dashboard on it), and a `status` tagged enum (`Ok` / `Error(message)` / `Timeout` / `Canceled`). Observer swap is atomic mid-call, so an exporter can be installed and torn down without disturbing in-flight work.

**The trap: `on_call` runs inline on the dispatch thread.** A native Rust observer that writes to disk or blocks on a network export *pins the dispatch thread* and throttles every call on the node. Push into your own bounded channel or lock-free ring instead. Every language binding already ships the trampoline that does this — `ObserverChannel` `try_send`s each event onto a 1024-slot bounded mpsc drained by a worker task; when it's full the event is **dropped** and the process-global `observer_dropped_total` counter increments. Alert on that counter: a rising `observer_dropped_total` means your observer is too slow, not that traffic fell.

Same shape in every binding: `setObserver` / `set_observer` / `SetObserver` plus `metricsSnapshot` / `metrics_snapshot` / `MetricsSnapshot`. Go additionally exposes `net_rpc_observer_dropped_total() -> u64` as an FFI symbol (and an `observer_dropped_total` field on the JSON snapshot) so monitoring doesn't pay the snapshot decode cost.

Server-side handler panics are caught, counted on `ServiceMetrics::handler_panics_total`, and surfaced to the caller as a server error — a panicking handler does not crash the node.

## Per-binding API

The typed surface ships in the **native binding**, not the SDK wrapper. Each language has the same five methods (`serve` / `call` / `callService` / `callStreaming` / `findServiceNodes`) plus the resilience helpers (`RetryPolicy` + `callWithRetry`, `HedgePolicy` + `callWithHedge`, `CircuitBreaker`).

### Rust (`net-mesh-sdk`, feature = "cortex")

```rust
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_rpc::{CallOptions, CallOptionsTyped, Codec};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

#[derive(Serialize, Deserialize)]
struct EchoSumRequest { text: String, numbers: Vec<i64> }
#[derive(Serialize, Deserialize)]
struct EchoSumResponse { echo: String, sum: i64 }

# async fn example() -> net_sdk::error::Result<()> {
let server = MeshBuilder::new("127.0.0.1:9001", &[0x42u8; 32])?.build().await?;
let client = MeshBuilder::new("127.0.0.1:9000", &[0x42u8; 32])?.build().await?;
// (handshake omitted — see SDK README's Mesh Streams example)

// Server side: register a typed handler. Returns a ServeHandle.
// On Drop the handle unregisters AND lets in-flight handlers
// complete (no abort).
// NOTE the `Codec` argument — `serve_rpc_typed` is (service, codec, handler).
let _handle = server.serve_rpc_typed(
    "echo_sum",
    Codec::Json,
    |req: EchoSumRequest| async move {
        Ok::<_, String>(EchoSumResponse {
            echo: req.text,
            sum: req.numbers.iter().sum(),
        })
    },
)?;

// Client side: typed call with a 200ms deadline.
// `call_typed` takes `CallOptionsTyped { raw: CallOptions, codec: Codec }` —
// the deadline lives on `.raw` and is an ABSOLUTE `Instant`, not a duration.
let opts = CallOptionsTyped {
    raw: CallOptions {
        deadline: Some(Instant::now() + Duration::from_millis(200)),
        ..Default::default()
    },
    ..Default::default()   // codec: Codec::Json
};
let resp: EchoSumResponse = client.call_typed(
    server.inner().node_id(),
    "echo_sum",
    &EchoSumRequest { text: "hi".into(), numbers: vec![1, 2, 3] },
    opts,
).await?;
# Ok(())
# }
```

**`Codec`** is `Json` (default) or `JsonPretty` (same wire format, indented — for human inspection of recorded traffic). Caller and server must agree out of band; there is no negotiation.

**`CallOptions` fields worth knowing** beyond `deadline`: `routing_policy` (below), `filter_unhealthy` (default `true` — skips candidates the `ProximityGraph` reports unhealthy; candidates with *no* proximity entry are **kept**, so a freshly-announced service isn't filtered out before pingwaves propagate), `trace_context` (`Some(TraceContext)` propagates W3C `traceparent` / `tracestate` to the server's `RpcContext::trace_context` — nRPC is transport-only here; both sides read/write via their own tracing backend), `stream_window_initial` / the request-direction mirror for flow control, and `cancel_token` (see § Cancellation).

Resilience helpers live in `net_sdk::mesh_rpc_resilience`: `RetryPolicy::default()` + `Mesh::call_with_retry`, `HedgePolicy::default()` + the per-target hedge helpers, `CircuitBreaker::new(CircuitBreakerConfig)`.

### TypeScript (`@net-mesh/core/mesh_rpc`)

The SDK's `MeshNode` wraps a `NetMesh` that nRPC consumes directly; the typed surface itself lives in the napi binding (sdk-ts doesn't yet re-export):

```typescript
import { MeshNode } from '@net-mesh/sdk'
import { classifyError, RpcServerError } from '@net-mesh/core/errors'
import {
  CircuitBreaker, HedgePolicy, NRPC_TYPED_BAD_REQUEST,
  RetryPolicy, TypedMeshRpc,
} from '@net-mesh/core/mesh_rpc'

interface EchoSumRequest  { text: string; numbers: number[] }
interface EchoSumResponse { echo: string; sum: number }

const server = await MeshNode.create({ bindAddr: '127.0.0.1:9001', psk })
const client = await MeshNode.create({ bindAddr: '127.0.0.1:9000', psk })
// (handshake omitted)

const serverRpc = TypedMeshRpc.fromMesh((server as any)._native)
const handle = serverRpc.serve<EchoSumRequest, EchoSumResponse>(
  'echo_sum',
  async (req) => ({ echo: req.text, sum: req.numbers.reduce((a, b) => a + b, 0) }),
)

const clientRpc = TypedMeshRpc.fromMesh((client as any)._native)
try {
  const reply = await clientRpc.call<EchoSumRequest, EchoSumResponse>(
    server.nodeId(), 'echo_sum',
    { text: 'hi', numbers: [1, 2, 3] },
    { deadlineMs: 200 },
  )
} catch (e) {
  const typed = classifyError(e)
  if (typed instanceof RpcServerError && typed.status === NRPC_TYPED_BAD_REQUEST) {
    // typed bad-request from the handler
  }
}
await handle.close()  // MUST close — node finalizers are non-deterministic
```

Streaming + resilience:

```typescript
const stream = await clientRpc.callStreaming<MyReq, MyChunk>(
  targetNodeId, 'tail', { tail: 'events' },
  { deadlineMs: 5_000, streamWindow: 8 },  // optional flow control
)
for await (const chunk of stream) { /* decoded MyChunk */ }
// stream.close() emits CANCEL; stream.grant(n) issues explicit credit.

const policy = new RetryPolicy({ maxAttempts: 4, initialBackoffMs: 50 })
await clientRpc.callWithRetry(targetNodeId, 'echo', req, policy)

const hedge = new HedgePolicy({ maxParallel: 3, hedgeDelayMs: 50 })
await clientRpc.callWithHedgeTo([nodeA, nodeB, nodeC], 'echo', req, hedge)

const breaker = new CircuitBreaker({ failureThreshold: 5, resetAfterMs: 1000 })
await breaker.call(() => clientRpc.call(targetNodeId, 'echo', req))
```

**vitest dual-module hazard:** the binding throws plain `Error` with the `nrpc:` prefix (NOT typed classes) to avoid two module-instance copies of `RpcServerError` failing `instanceof`. **Always classify at the catch site** — don't rely on `instanceof` against an exception thrown inside the binding.

### Python (`net.mesh_rpc`, feature = "cortex")

```python
from net import NetMesh
from net.mesh_rpc import (
    CircuitBreaker, HedgePolicy, NRPC_TYPED_BAD_REQUEST,
    RetryPolicy, RpcServerError, TypedMeshRpc, classify_error,
)

server = NetMesh("127.0.0.1:9001", "42" * 32)
client = NetMesh("127.0.0.1:9000", "42" * 32)
# (handshake omitted)

server_rpc = TypedMeshRpc.from_mesh(server)
def echo_sum(req: dict) -> dict:
    return {"echo": req["text"], "sum": sum(req["numbers"])}

# ServeHandle is a context manager — `with` ensures unregister on exit.
with server_rpc.serve("echo_sum", echo_sum):
    client_rpc = TypedMeshRpc.from_mesh(client)
    try:
        reply = client_rpc.call(
            server.node_id(), "echo_sum",
            {"text": "hi", "numbers": [1, 2, 3]},
            opts={"deadline_ms": 200},
        )
    except RpcServerError as e:
        if classify_error(e) == "server_error":
            # typed handler bad-request
            ...
```

**GIL note:** synchronous calls release the GIL across `runtime.block_on(...)` so other Python threads can run. Handler callbacks dispatch under `tokio::task::spawn_blocking` so GIL acquisition doesn't starve the runtime. The typed surface defaults to JSON; if you want zero-copy bytes, use `net.MeshRpc` directly (the raw layer the typed wrapper sits on).

### Go

Two Go trees exist and they are not the same thing:

- **The shipped module**, `github.com/ai-2070/net/go`, source at `go/`. This is
  what `go get` gives you, and `go/mesh_rpc_typed.go` carries the typed surface.
- **A reference implementation** at `net/crates/net/bindings/go/net/`, with no
  `go.mod` — meant to be vendored or copied into your own module. It covers
  some surfaces the shipped module does not.

The C-ABI cdylib `libnet_rpc` is built from `net/crates/net/bindings/go/rpc-ffi/`.

```go
import "github.com/ai-2070/net/go"

rpc, _ := net.NewMeshRpc(nodeArc)           // takes Arc<MeshNode> from compute-ffi
defer rpc.Close()

// Server side
handle, _ := rpc.Serve("echo_sum", func(req []byte) ([]byte, error) {
    var r struct { Text string; Numbers []int64 }
    if err := json.Unmarshal(req, &r); err != nil { return nil, err }
    sum := int64(0); for _, n := range r.Numbers { sum += n }
    return json.Marshal(struct{ Echo string; Sum int64 }{r.Text, sum})
})
defer handle.Close()

// Client side
ctx, cancel := context.WithTimeout(context.Background(), 200 * time.Millisecond)
defer cancel()
reply, err := rpc.Call(ctx, targetNodeID, "echo_sum", reqBytes)
if err != nil {
    var rpcErr *net.RpcError
    if errors.As(err, &rpcErr) && rpcErr.Kind == net.RpcKindServerError { ... }
}

// Streaming with ctx-cancel watcher
stream, _ := rpc.CallStreaming(ctx, targetNodeID, "tail", reqBytes,
    net.StreamOptions{Window: 8})
defer stream.Close()
for {
    chunk, err := stream.Recv()
    if errors.Is(err, net.ErrStreamDone) { break }
    if err != nil { return err }
    process(chunk)
}
```

Pure-Go resilience helpers (`RetryPolicy` + `CallWithRetry`, `HedgePolicy` + `CallWithHedge`, `CircuitBreaker`) live in `net/crates/net/bindings/go/net/resilience.go` — the reference tree, **not** the shipped module, so `go get github.com/ai-2070/net/go` does not bring them. Vendor that file or write your own. ABI version drift is detected via `net.ABIVersion()` vs `net.ExpectedABIVersion`, currently `0x0004`.

### C — a separate header and a separate library

nRPC is **not** in `net.h`, and that is a header-layout fact rather than a gap: the C SDK is ten headers over six cdylibs. nRPC lives in its own pair.

Include `net/crates/net/include/net_rpc.h` and link `libnet_rpc`, built with `cargo build --release -p net-rpc-ffi`. The header is the canonical drop-in for C, C++, Zig, Swift, JNI and anything else with a C ABI — it is not Go-specific, though the Go binding is its most-exercised consumer. It carries the full surface: `net_rpc_call` and the service/header/streaming/cancellable variants, `net_rpc_serve_streaming`, `net_rpc_find_service_nodes`, plus `net_rpc_abi_version()` / `net_rpc_check_abi_version()` for the version handshake.

Call `net_rpc_check_abi_version(NET_RPC_ABI_VERSION)` at process init and refuse to continue on mismatch. See `net/crates/net/include/README.md` § nRPC for the entry-point listing and error codes.

---

## Resilience helpers — when to reach for which

| Helper            | Use when                                                                          |
| ----------------- | --------------------------------------------------------------------------------- |
| `call_with_retry` | Transient network blips / target restarts. See the exact default predicate below — it is **not** "retry `no_route`". |
| `call_with_hedge` | p99 latency tail matters more than wasted bandwidth. Fires N parallel attempts on a delay; first success wins. |
| `CircuitBreaker`  | Repeatedly-failing target should be skipped without cost. Closed → open after threshold; half-open probe after cooldown. |

Stack them: `breaker.call(() => callWithRetry(...))` is the typical "give up fast on a wedged target, but tolerate single retries" combo.

### The default retry predicate (verbatim from source)

`RetryPolicy::default()` is 3 attempts, 50 ms initial backoff, ×2 growth, 1 s cap, full jitter on (`[0.5, 1.0]` multiplier, to decorrelate retry storms). The predicate is `default_retryable` (`sdk/src/mesh_rpc_resilience.rs`):

| `RpcError` | Retried? | Why |
|---|---|---|
| `Timeout` | ✅ | transient |
| `Transport(_)` | ✅ | transient |
| `ServerError { status }` where status ∈ {`Internal`, `Backpressure`, `Timeout`} | ✅ | server-side transient |
| `NoRoute` | ❌ | nobody serves it; retrying the same instant won't change that |
| `Codec { .. }` | ❌ | caller-fixable local bug (wrong codec, schema drift) |
| `ServerError` for `Application` / `NotFound` / `Unauthorized` / `UnknownVersion` | ❌ | terminal or caller-fixable |
| `CapabilityDenied { .. }` | ❌ | the target's signed policy denies you; only a new announcement changes it |
| `Cancelled` | ❌ | you asked it to stop |

Override with `RetryPolicy::with_retryable(|e| ...)` when you genuinely want a different classification (e.g. retry a specific application status). **`opts.raw.deadline` is an absolute `Instant` and does not advance across retries** — total wall-clock is bounded by the initial deadline plus the sum of backoffs, so a tight deadline silently caps `max_attempts`.

**Don't** wrap `call_with_retry` around itself. **Don't** retry `codec_*` errors — they're caller bugs, not transient. **Don't** install a breaker that opens on `no_route` alone — `no_route` fires before any handshake, so a flapping breaker on it just blocks legitimate retries.

---

## Throughput tuning — ingress/egress batching

nRPC throughput is bounded by the shared mesh receive loop, not the handler. Two opt-in batching paths exist; **both default off**.

- **`recvmmsg` ingress batching** — build feature `batched-ingress` + runtime `MeshNodeConfig::batched_ingress = true` routes the mesh receive loop through the Linux `recvmmsg` path, handing a whole syscall batch over the channel at once instead of one packet per `blocking_send`. The field exists **only** when the crate is built `--features batched-ingress` (Linux-only effect); default `false`. Turn it on for high-QPS ingress where the per-packet channel-hop tax dominates.
- **`sendmmsg` egress batching** — a per-mesh group-by-destination drain coalesces relayed packets to the same peer into one `sendmmsg` syscall. It's **disabled on the originating fast path** on purpose: request/response is latency-bound with send-queue depth ≈ 1, so send-batching there adds latency without throughput. The win is real only for concurrent *relayed* traffic between two peers.

**The scaling wall is the recv pipeline.** `nrpc_qps` scales `c1 → c16` at ~4×, not 16× — the bottleneck is the single-consumer `recv → AEAD decrypt → bridge task → fold mutex` chain, not the send path or the handler. Batching shaves syscall overhead but does not move this wall (the ack-piggyback protocol that does is a future release). Don't promise linear QPS scaling from raising concurrency alone.

---

## AI tool calling — one identifier, one source of truth

Any typed nRPC service can also expose itself as an **LLM-callable tool**. The identity collapse is the design: a tool registered as `web_search` **is** the nRPC service at `web_search` **is** the announcement carrying the `ai-tool:web_search` and `nrpc:web_search` capability tags. One identifier, no separate tool registry to keep in sync.

### Declaring a tool

```rust
use net_sdk::macros::tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(JsonSchema, Deserialize, Serialize)]
struct WebSearchArgs { query: String }
#[derive(JsonSchema, Deserialize, Serialize)]
struct WebSearchReply { hits: Vec<String> }

#[tool(name = "web_search", description = "Search the web.", tag = "web")]
async fn web_search(args: WebSearchArgs) -> Result<WebSearchReply, String> {
    Ok(WebSearchReply { hits: run_query(&args.query).await.map_err(|e| e.to_string())? })
}

let handle = web_search_register(&mesh)?;   // generated alongside the fn
```

`net_sdk::macros` is **feature-gated on `macros`** — without that feature the attribute doesn't exist and you build the descriptor by hand (below). It's a separate feature because it drags in the proc-macro2 / syn / quote build cost.

The `#[tool]` attribute expects `async fn <name>(<req>: Req) -> Result<Resp, _>`; **both `Req` and `Resp` must implement `schemars::JsonSchema`** so the macro can derive the input/output schemas. It generates a sibling `<fn_name>_descriptor()` returning a `ToolDescriptor` and `<fn_name>_register(mesh)` that calls `mesh.serve_tool(..)`. Attribute surface: `name`, `description`, `version`, `stateless`, `estimated_time_ms`, `tags`. Without the macro, build the descriptor by hand with `metadata_for::<Req, Resp>("name")` + the builder.

**Registration is atomic.** The descriptor insert into the capability fold, the `nrpc:<tool>` service registration, the `ai-tool:<tool>` discovery tag, and the lazily auto-installed `tool.metadata.fetch` RPC (for fetching oversized JSON Schemas that don't fit an announcement) all succeed together or none do. Dropping the handle reverses the descriptor insert and the handler registration the same way.

### Discovering and calling

```rust
let tools = mesh.list_tools(None);            // sync — reads the local fold, no RPC fan-out
let reply: WebSearchReply = mesh.call_tool("web_search", &args).await?;
```

`list_tools(Option<&TagMatcher>)` is **synchronous** — the capability fold already aggregates every node's announcements, so discovery is an in-memory read, not a query. `watch_tools` is the streaming sibling; `tool.watch` is its remote form (next section).

### Streaming tools — the `ToolEvent` envelope

Serve with `serve_tool_streaming` (handler returns a `Stream<Item = ToolEvent>`), call with `call_tool_streaming`. The envelope is a tagged enum, `#[serde(tag = "type", rename_all = "snake_case")]`:

| Variant | Payload | Meaning |
|---|---|---|
| `start` | `tool_id`, optional `call_id`, optional `metadata` | fires once on stream open; `call_id` correlates when an agent has several tool calls in flight |
| `progress` | optional `pct` (`0.0..=100.0`), optional `message` | coarse progress for spinner UIs |
| `delta` | `data` (tool-defined JSON; `{"token": …}` / `{"chunk": "<base64>"}` are the conventions) | partial output |
| `result` | `data` | **terminal** success |
| `error` | `code`, `message` | **terminal** failure |

**Exactly one terminal envelope per stream** — `result` OR `error`, never both. A handler that ends without one gets `ToolEvent::Error { code: "missing_terminal", .. }` synthesized by the SDK, so callers can rely on every stream terminating properly. Unary tools synthesize a single `result` under the hood, which is what lets one adapter handle both.

### Format translators — descriptor → provider schema

`net_sdk::tool::formats` (Rust), `@net-mesh/core/tool` (Node), `net.tool` (Python), plus the Go equivalent, ship **pure-function** translators in both directions:

- `to_<provider>_tool(&ToolDescriptor) -> Value` — descriptor → the provider's tool-definition shape, to populate the `tools` array on a request (`openai::to_openai_tool`, `anthropic::to_anthropic_tool`).
- `lower_<provider>_tool_call(&Value) -> Result<ToolCallSpec, _>` — parse the provider's reply (OpenAI `tool_calls[]`, Anthropic `tool_use` block) into a `ToolCallSpec` you hand straight to `mesh.call_tool(spec.name, &spec.arguments)`.

```ts
import { openai, listTools } from "@net-mesh/core/tool";
const openaiTools = listTools(mesh).map(openai.toOpenaiTool);
// model replies → openai.lowerOpenaiToolCall(call) → mesh.callTool(spec.name, args)
```

**No transitive dependency on any provider SDK** — translators emit plain JSON and you wire it into your own model client. A descriptor with no `input_schema` lowers to an empty-object schema (`{"type":"object","properties":{}}`) rather than `null`, because provider strict-mode validators reject a null parameter schema but accept empty-properties as "no arguments." Cross-language byte-equality is pinned by golden vectors in CI.

## Watching tool discovery remotely — `tool.watch`

Local tool discovery is `list_tools(matcher)` (snapshot) and `watch_tools(matcher, interval)` (event-driven deltas off the node's own mesh-replicated capability fold). **`tool.watch` is the remote form**: a server-streaming nRPC service that relays another node's fold deltas to you.

- **You almost never install it explicitly.** Every `serve_tool*` path auto-installs it, idempotently, for the lifetime of the `Mesh`. `Mesh::serve_tool_watch()` exists for the one case that isn't covered: a node acting as a pure **discovery relay** — it serves no tools of its own but lets remote subscribers stream deltas diffed from its fold.
- **Protocol.** Send one JSON `WatchToolsRequest` (`{ matcher?, interval_ms? }`), receive one JSON `ToolWatchFrame` per chunk. Open it with `call_streaming_typed::<WatchToolsRequest, ToolWatchFrame>`. `matcher: None` watches every tool the serving node sees; `interval_ms` is a debounce *ceiling*, not a poll — omit it for pure event-driven.
- **Frames** are `{"type": "added"|"removed"|"node_count_changed"|"resync"}`, the same JSON shape the FFI bindings already use for `ToolListChange`.
- **The resync contract is the part that bites.** Deltas ride a bounded per-subscriber buffer (`TOOL_WATCH_SUBSCRIBER_BUFFER = 64`). A subscriber that falls behind has its queued deltas **dropped** and gets one `Resync` frame instead — a delta is never lost *silently*, but it is lost. On `Resync` you must discard your accumulated view and re-baseline from your **own local `list_tools`** (the fold is mesh-replicated; there is no remote list service to call). Frames after a resync resume normal delta semantics, and a delta already reflected in your fresh baseline is expected — **make your apply idempotent**.
- **Auth** rides the ordinary callee-side nRPC capability gate on `nrpc:tool.watch`.

Binding surfaces mirror the Rust `Mesh::watch_tools(matcher, interval)`: TS `watchTools(mesh, options)` (an `AsyncIterable<ToolListChange>` — `for await (const change of watchTools(mesh))`), Python `watch_tools`, Go `WatchTools`, FFI `net_rpc_watch_tools`. **All are event-driven** — the Go binding's old 1 s poll is gone. A sub-millisecond `WatchOptions.Interval` in Go rounds up to 1 ms rather than truncating to 0 (which would have meant "no staleness ceiling").

---

## Typed bindings from discovered tools — `net-mesh typegen`

nRPC has no IDL and needs none — the wire is schemaless JSON and each side ships its own typed serializer. An **optional** codegen path covers the AI-tool case: `net-mesh typegen` walks the capability fold for `ai-tool:*` tags, fetches each matching descriptor's metadata, and emits typed bindings so a caller gets compile-time types for a tool it discovered at runtime. Codegen is a convenience over the same wire RPC — a generated call lands identically to a hand-written `call_typed`.

Output is one module per tool: the tool's JSON Schema lowers to TypeScript interfaces (`--language ts`) or Pydantic v2 models (`--language python`), plus a typed call helper (`callAcmeWebSearch(mesh, request)` / `call_acme_web_search(mesh, request)`) and a `…Meta` constant carrying the descriptor (tool id, version, streaming flag, tags). Bindings are cross-language by construction — a Python agent calling a TypeScript tool calling a Go server is the same wire shape as Rust→Rust.

| Command | Does |
|---|---|
| `net-mesh typegen generate --language ts --tag weather --out ./generated` | generate from live discovery, filtered by tag (`--tag` repeatable; ANY match) |
| `net-mesh typegen generate --language python --tool acme.web-search --out ./generated` | generate for explicit tool ids (`--tool` repeatable; exact match) |
| `net-mesh typegen generate --from-snapshot <file> …` | regenerate from a pinned snapshot — no mesh query, hermetic CI |
| `net-mesh typegen snapshot --tag weather --out <file>` | capture currently-discoverable descriptors into a versioned snapshot |
| `net-mesh typegen diff --from <a> --to <b> [--exit-code]` | added/removed tools + schema deltas; `--exit-code` exits 14 on a BREAKING change (CI gate) |

The actual flags are singular-and-repeatable (`--tag`, `--tool`) and `diff` takes `--from` / `--to`, **not** positional args. Ships behind the `cli` feature flag. Source: `cli/src/commands/typegen/`. **Full `generate` / `snapshot` / `diff` flag surface + exit codes: `cli.md`.**

---

## Cross-binding contract

The canonical interop contract — used by every binding's wire-format compat test — is the `cross_lang_echo_sum` service. Same JSON shape, same status codes, same error prefixes across Rust / Node / Python / Go.

Shared fixture: `net/crates/net/tests/cross_lang_nrpc/golden_vectors.json`. Every binding loads it and runs:

| Binding | Test file                                                  |
| ------- | ---------------------------------------------------------- |
| Rust    | `net/crates/net/tests/integration_nrpc_cross_lang.rs`      |
| Node    | `net/crates/net/bindings/node/test/cross_lang_compat.test.ts` |
| Python  | `net/crates/net/bindings/python/tests/test_cross_lang_compat.py` |

If a binding's encoder, status-code map, or error-prefix convention drifts, that binding's compat test fails in its own CI. The fixture is versioned via `abi_version_expected` mirroring `NET_RPC_ABI_VERSION = 0x0001` from `bindings/go/rpc-ffi/src/lib.rs` — bumping the ABI invalidates the fixture and forces every binding's compat test to update.

True subprocess-based interop tests (Node caller → Rust server, Python caller → Rust server, Node ↔ Python) are out of scope. When Cargo can portably orchestrate Node / Python subprocesses AND both bindings ship pre-built native modules in CI, add a `tests/cross_lang_nrpc.rs` driver gated on `CROSS_LANG_NRPC=1` + `NET_NODE_BUILT=1` / `NET_PYTHON_BUILT=1`.

---

## Common mistakes

- **Using nRPC for fire-and-forget broadcast.** That's the bus. nRPC has per-call subscription overhead pub/sub doesn't.
- **Forgetting to close the `ServeHandle`.** TS finalizers are non-deterministic; always call `.close()`. Python: use `with`. Rust: `Drop` is automatic. Go: `defer handle.Close()`.
- **`instanceof` in TS catch sites.** The binding throws plain `Error` with prefix; classify at the catch site via `classifyError(e)`.
- **Retrying `codec_decode` errors.** They're caller bugs — wrong type, malformed JSON. Retry just burns the same error.
- **Defining custom status codes in the protocol band (`0x0000..=0x7FFF`).** Use the application band (`0x8000..=0xFFFF`).
- **Using `call_service` when `call` is right.** `call_service` does service discovery on every call. If you have a stable target, cache its node id and use `call`.
- **Configuring a deadline shorter than the handler's tail latency.** Caller observes `nrpc:timeout`; handler still runs to completion (no preemption from the bus side, just CANCEL emitted to the server which the handler may or may not observe).
- **Streaming without `streamWindow` for high-rate producers.** Auto-grant covers the common case (1 credit per chunk delivered) but explicit `grant(window/2)` cadence is better when chunks are uniform-sized and you want fewer round-trips.
- **Running `serve` on a node that's not yet handshaken.** Returns a handle but no calls land. Verify with `findServiceNodes(service)` from the caller's side — empty list = capability hasn't propagated.

---

## Where to look in source

- **Rust core** — `net/crates/net/src/adapter/net/mesh_rpc.rs` (client surface), `net/crates/net/src/adapter/net/cortex/rpc.rs` (server fold), `net/crates/net/src/adapter/net/mesh_rpc_metrics.rs` (per-service counters + Prometheus formatter).
- **Rust SDK** — `net/crates/net/sdk/src/mesh_rpc.rs` (typed wrappers), `net/crates/net/sdk/src/mesh_rpc_resilience.rs` (`RetryPolicy` / `HedgePolicy` / `CircuitBreaker`).
- **Node binding** — `net/crates/net/bindings/node/src/mesh_rpc.rs` (napi cdylib), `net/crates/net/bindings/node/mesh_rpc.ts` (wrapper class), `net/crates/net/bindings/node/errors.ts` (`classifyError`).
- **Python binding** — `net/crates/net/bindings/python/src/mesh_rpc.rs` (PyO3 cdylib), `net/crates/net/bindings/python/python/net/mesh_rpc.py` (Python wrapper).
- **Go C-ABI** — `net/crates/net/bindings/go/rpc-ffi/src/lib.rs` (cdylib), `net/crates/net/bindings/go/net/mesh_rpc.go` (reference cgo wrapper), `net/crates/net/bindings/go/net/resilience.go` (pure-Go resilience helpers).
- **Cross-binding contract** — `net/crates/net/tests/cross_lang_nrpc/golden_vectors.json` (shared fixture), the three binding compat tests (paths above).
- **Tool discovery + `tool.watch`** — `net/crates/net/sdk/src/tool.rs` (`list_tools` / `watch_tools` / `serve_tool_watch`), `net/crates/net/src/adapter/net/cortex/tool.rs` (`TOOL_WATCH_SERVICE`, `WatchToolsRequest`, `ToolWatchFrame`).
- **Org-protected calls** — `net_sdk::mesh_rpc::OrgProofIntent` on `CallOptions` is the low-level seam under `mesh.org(..).call(..)`; use it when you need an exact provider, a specific grant, or an unusual proof TTL. See `org.md`.
- **READMEs** — `README.md` § nRPC (top-level concept + cross-binding spec); per-binding READMEs each have an `## nRPC` section with language-idiomatic examples.

## Further reading

- [Typed RPC with nRPC](https://ai2070.net/docs/guides/nrpc)
- [Recover a Failed Workflow](https://ai2070.net/docs/guides/recover-failed-workflow)
