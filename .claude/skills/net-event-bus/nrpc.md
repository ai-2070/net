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
| `breaker_open`  | `BreakerOpenError` (resilience helper) |

Each binding ships a `classifyError(e)` / `classify_error(e)` helper that maps a raw `nrpc:`-prefixed exception to the typed subclass. Used at catch sites where `instanceof` discrimination is awkward (e.g. fallback paths where the native module wasn't built; vitest dual-module-instance hazard).

---

## Per-binding API

The typed surface ships in the **native binding**, not the SDK wrapper. Each language has the same five methods (`serve` / `call` / `callService` / `callStreaming` / `findServiceNodes`) plus the resilience helpers (`RetryPolicy` + `callWithRetry`, `HedgePolicy` + `callWithHedge`, `CircuitBreaker`).

### Rust (`net-sdk`, feature = "cortex")

```rust
use net_sdk::mesh::{Mesh, MeshBuilder};
use net_sdk::mesh_rpc::CallOptions;
use serde::{Deserialize, Serialize};
use std::time::Duration;

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
let _handle = server.serve_rpc_typed(
    "echo_sum",
    |req: EchoSumRequest| async move {
        Ok::<_, String>(EchoSumResponse {
            echo: req.text,
            sum: req.numbers.iter().sum(),
        })
    },
)?;

// Client side: typed call with a 200ms deadline.
let opts = CallOptions::default().with_deadline(Duration::from_millis(200));
let resp: EchoSumResponse = client.call_typed(
    server.inner().node_id(),
    "echo_sum",
    &EchoSumRequest { text: "hi".into(), numbers: vec![1, 2, 3] },
    opts,
).await?;
# Ok(())
# }
```

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

### Go (downstream — reference at `bindings/go/net/`)

The Go binding ships **downstream** (no Go module in the upstream net repo). The C-ABI cdylib `libnet_rpc` lives at `bindings/go/rpc-ffi/`; the reference cgo wrapper at `bindings/go/net/mesh_rpc.go` documents the consumer-side API:

```go
import "ai2070.com/net"

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

Pure-Go resilience helpers (`RetryPolicy` + `CallWithRetry`, `HedgePolicy` + `CallWithHedge`, `CircuitBreaker`) live in `bindings/go/net/resilience.go`. ABI version drift is detected via `net.ABIVersion()` vs `net.ExpectedABIVersion = 0x0001`.

### C — not exposed in `net.h`

The C SDK at `net.h` does **not** expose the nRPC surface. The C ABI lives in a separate cdylib (`libnet_rpc` from `bindings/go/rpc-ffi/`) primarily consumed by the Go binding but callable from any C-ABI consumer. There's no shipped `.h` today — the canonical signatures live in `bindings/go/net/mesh_rpc.go` (cgo include block, drop-in template) and `bindings/go/rpc-ffi/src/lib.rs`. See `net/crates/net/include/README.md` § nRPC for the entry-point listing + error codes.

---

## Resilience helpers — when to reach for which

| Helper            | Use when                                                                          |
| ----------------- | --------------------------------------------------------------------------------- |
| `call_with_retry` | Transient network blips / target restarts. Default predicate retries `no_route` + `transport`; skips terminal `server_error` / `codec_*`. |
| `call_with_hedge` | p99 latency tail matters more than wasted bandwidth. Fires N parallel attempts on a delay; first success wins. |
| `CircuitBreaker`  | Repeatedly-failing target should be skipped without cost. Closed → open after threshold; half-open probe after cooldown. |

Stack them: `breaker.call(() => callWithRetry(...))` is the typical "give up fast on a wedged target, but tolerate single retries" combo.

**Don't** wrap `call_with_retry` around itself. **Don't** retry `codec_*` errors — they're caller bugs, not transient. **Don't** install a breaker that opens on `no_route` alone — `no_route` fires before any handshake, so a flapping breaker on it just blocks legitimate retries.

---

## Throughput tuning — ingress/egress batching

nRPC throughput is bounded by the shared mesh receive loop, not the handler. Two opt-in batching paths exist; **both default off**.

- **`recvmmsg` ingress batching** — build feature `batched-ingress` + runtime `MeshNodeConfig::batched_ingress = true` routes the mesh receive loop through the Linux `recvmmsg` path, handing a whole syscall batch over the channel at once instead of one packet per `blocking_send`. The field exists **only** when the crate is built `--features batched-ingress` (Linux-only effect); default `false`. Turn it on for high-QPS ingress where the per-packet channel-hop tax dominates.
- **`sendmmsg` egress batching** — a per-mesh group-by-destination drain coalesces relayed packets to the same peer into one `sendmmsg` syscall. It's **disabled on the originating fast path** on purpose: request/response is latency-bound with send-queue depth ≈ 1, so send-batching there adds latency without throughput. The win is real only for concurrent *relayed* traffic between two peers.

**The scaling wall is the recv pipeline.** `nrpc_qps` scales `c1 → c16` at ~4×, not 16× — the bottleneck is the single-consumer `recv → AEAD decrypt → bridge task → fold mutex` chain, not the send path or the handler. Batching shaves syscall overhead but does not move this wall (the ack-piggyback protocol that does is a future release). Don't promise linear QPS scaling from raising concurrency alone.

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
- **Node binding** — `net/crates/net/bindings/node/src/mesh_rpc.rs` (napi cdylib), `net/crates/net/bindings/node/mesh_rpc.js` (JS wrapper class), `net/crates/net/bindings/node/errors.js` (`classifyError`).
- **Python binding** — `net/crates/net/bindings/python/src/mesh_rpc.rs` (PyO3 cdylib), `net/crates/net/bindings/python/python/net/mesh_rpc.py` (Python wrapper).
- **Go C-ABI** — `net/crates/net/bindings/go/rpc-ffi/src/lib.rs` (cdylib), `net/crates/net/bindings/go/net/mesh_rpc.go` (reference cgo wrapper), `net/crates/net/bindings/go/net/resilience.go` (pure-Go resilience helpers).
- **Cross-binding contract** — `net/crates/net/tests/cross_lang_nrpc/golden_vectors.json` (shared fixture), the three binding compat tests (paths above).
- **READMEs** — `net/crates/net/README.md` § nRPC (top-level concept + cross-binding spec); per-binding READMEs each have an `## nRPC` section with language-idiomatic examples.
