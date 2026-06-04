# Typed RPC with nRPC

nRPC layers request/response semantics on top of channels. Where the bus is fire-and-forget — you publish, subscribers pull — nRPC gives you a typed call that takes arguments, returns a result, and threads identity, capability targeting, cancellation, and tracing through the same primitive.

You use nRPC when the natural shape of an interaction is a call: a client asks a service for something, the service computes, the client gets an answer. You stay on the event bus when the natural shape is a stream that producer and consumer don't synchronize on. Both are first-class; they share the same routing, the same identity, and the same encryption.

## The shape of a call

Define a service with a typed request and response. The Rust SDK gives you a derive macro that handles the wire encoding, the dispatch glue, and the request/response correlation:

```rust
use net_sdk::{TypedMeshRpc, RpcRequest, RpcResponse};

#[derive(RpcRequest)]
struct EchoArgs {
    message: String,
}

#[derive(RpcResponse)]
struct EchoReply {
    echoed: String,
    received_at: u64,
}

#[derive(TypedMeshRpc)]
#[rpc(name = "demo.echo")]
struct EchoService;
```

The service name (`"demo.echo"`) is a string that uniquely identifies the method on the mesh. Names use the same hierarchical convention as channels — `module.method` is conventional, but the grammar is yours.

## Serving and calling — unary

A server registers a handler against the service name and starts a worker. The worker pulls requests off the underlying channel, deserializes the args, runs the handler, and publishes the response back to the caller's reply channel:

```rust
use net_sdk::serve_rpc;

let handle = serve_rpc(&mesh, EchoService, |args: EchoArgs| async move {
    Ok(EchoReply {
        echoed: args.message,
        received_at: now_unix_ms(),
    })
}).await?;
```

A client calls the service by name:

```rust
use net_sdk::call_typed;

let reply: EchoReply = call_typed(
    &mesh,
    EchoService,
    EchoArgs { message: "hello".into() },
).await?;
```

If there's no server registered for the service, the call fails with `RpcError::NoServer`. If multiple servers are registered, the call goes to one of them — the SDK picks by load and proximity, biased toward the nearest healthy advertiser. Handler errors come back as typed `RpcAppError(code, detail)`; panics are caught and surfaced as `RpcError::Panic`.

## Streaming — server-streaming, client-streaming, duplex

Four call shapes share one wire and one typed surface. The unary case above is the simplest; the three streaming shapes layer on the same primitive.

**Server-streaming.** The handler returns a `Stream` instead of a single value. Useful for model inference with token streaming, file downloads with progress, or any case where one response would be too coarse:

```rust
use futures::stream;

let handle = serve_rpc_streaming_typed(&mesh, TokenService, |args| async move {
    Ok(stream::iter(args.tokens.into_iter().map(Ok)))
}).await?;

let mut stream = call_streaming_typed(&mesh, TokenService, args).await?;
while let Some(token) = stream.next().await {
    print!("{}", token?);
}
```

**Client-streaming.** The caller sends a sequence of request chunks; the server returns one terminal response after the stream closes:

```rust
let call = call_client_stream_typed(&mesh, UploadService, ()).await?;
for chunk in file_chunks {
    call.send(chunk).await?;
}
let receipt: UploadReceipt = call.finish().await?;
```

The server-side handler receives a `TypedRequestStream`; decode failure on any chunk surfaces as `RpcAppError(NRPC_TYPED_BAD_REQUEST, ...)` so callers observe typed Application status instead of generic Internal.

**Duplex.** Both sides send sequences of typed messages independently. Most useful for interactive sessions and long-running coordination:

```rust
let call = call_duplex_typed(&mesh, ChatService, ()).await?;
let (sink, mut stream) = call.into_split();

tokio::spawn(async move {
    for msg in outgoing { sink.send(msg).await.ok(); }
    sink.finish_sending().await.ok();
});

while let Some(incoming) = stream.next().await {
    handle(incoming?);
}
```

All four shapes ship across Rust, Node, Python (sync + async), and Go with the same typed wrappers and the same wire contract. A Python client can drive a Go duplex handler can talk to a Node server-streaming handler — cross-language interop is pinned by shared golden vectors in CI.

## Capability-targeted calls

The interesting cases come when you don't just want *any* server — you want a server that matches some capability. nRPC carries a serialized predicate alongside the call. Servers evaluate the predicate against their local capability set; mismatched servers refuse the call without invoking the handler:

```rust
use net_sdk::{call_typed_where, predicate};

let pred = predicate!("hardware.gpu" exists && "software.cuda" >= "12.0");

let reply: InferenceReply = call_typed_where(
    &mesh,
    InferenceService,
    InferenceArgs { prompt: "..." },
    pred,
).await?;
```

The predicate ships in a `net-where:` request header. The receiver's predicate evaluator runs against its own capability set; if it matches, the handler runs; if not, the call is routed elsewhere or fails with `RpcError::NoMatchingServer`. This is how you build a service mesh without standing up a separate one — "route this call to a node with these capabilities" is a primitive, with no sidecar, no service-discovery layer, and no separate authentication step.

## Cancellation

Cancellation is a first-class substrate primitive. Reserve a token from the mesh, pair it with a call, and `cancel(token)` from any thread aborts the in-flight call cleanly — sending a CANCEL on the wire and unblocking the caller. Honored uniformly by every call shape (unary, server-streaming, client-streaming, duplex):

```rust
let token = mesh.reserve_cancel_token();

let call = tokio::spawn({
    let mesh = mesh.clone();
    async move {
        call_typed_with_options(
            &mesh,
            ExpensiveService,
            args,
            CallOptions::default().with_cancel_token(token),
        ).await
    }
});

// Some time later, from anywhere:
mesh.cancel(token);
```

The substrate registers the token's abort handle at construction and removes it on resolution. A cancel that arrives *before* the call's abort handle is registered (the gap between `reserve` and call construction) is held as a latched flag on an orphan entry; when the call later registers, it observes the flag and aborts immediately. Token reservations that never get used are GC'd after an orphan TTL.

Idiomatic surfaces wrap the primitive in each binding:

- **Node:** `AbortSignal` end-to-end. Pass `signal` in `CallOptions`; `signal.abort()` cancels.
- **Python:** `Cancellable` class. Construct, pass via `cancel=`, call `.cancel()`.
- **Go:** `context.Context`. Pass `ctx` to the call; `cancel()` cancels.

All three lower to the same substrate token, so a TS client cancelling a call to a Python server is wire-equivalent to a Python client cancelling a call to a Go server. Power users can reserve tokens directly via the raw substrate surface for cross-call cancel sharing.

## Observers and metrics

Every typed-RPC handle exposes an observer hook and a metrics snapshot. The observer fires per-call with a typed event describing what happened; the snapshot reports cumulative counters across every call the handle has served or made:

```rust
rpc.set_observer(|event: Arc<RpcCallEvent>| {
    metrics.record(event.service, event.duration, event.status);
});

let snap = rpc.metrics_snapshot();
println!("served: {}, dropped observer events: {}",
    snap.calls_served, snap.observer_dropped_total);
```

`RpcCallEvent` is a tagged-union carrying the service name, direction, request/response byte counts, latency, and status (`Ok` / `Error(message)` / `Timeout` / `Canceled`). The observer-handle swap is atomic mid-call, so a Prometheus exporter or a structured-log sink can be installed and torn down without disrupting in-flight work.

Critically, observer dispatch is **bounded-mpsc-buffered**, not synchronous. The substrate emits each event onto a per-mesh 1024-slot channel via `try_send`; a single worker drains the channel and pumps events to the registered consumer. A slow callback — a disk-flushing logger, a network-pinning metrics exporter — can't pin the substrate's dispatch thread. If the channel is full, the substrate increments `observer_dropped_total` and moves on; the counter is exposed on every binding's metrics snapshot so dashboards can detect when observer back-pressure is happening.

The same shape exists in every binding: `setObserver` / `set_observer` / `SetObserver` plus `metricsSnapshot` / `metrics_snapshot` / `MetricsSnapshot`. Go consumers additionally get a `net_rpc_observer_dropped_total() -> u64` FFI symbol for monitoring without paying the JSON-decode cost on the snapshot path.

## Resilience

`call_typed` retries on transient failures, applies a configurable timeout, and surfaces typed errors for everything else. The SDK ships a resilience layer that wraps the base call with circuit-breaking and bounded retries:

```rust
use net_sdk::mesh_rpc_resilience::{Resilience, RetryPolicy};

let resilient = Resilience::new(&mesh)
    .with_retry(RetryPolicy::exponential(3, Duration::from_millis(50)))
    .with_timeout(Duration::from_secs(5));

let reply: EchoReply = resilient.call(EchoService, args).await?;
```

The retry policy applies to network errors and no-server errors; it does *not* apply to handler errors (those are returned as-is). Timeouts cancel the in-flight call and return `RpcError::Timeout`.

## AI tool calling

Every typed nRPC service can also expose itself as an LLM-callable tool. A tool registered as `web_search` IS the nRPC service at channel `nrpc:web_search.requests` IS the announcement carrying the `ai-tool:web_search` capability tag — one identifier, one source of truth, no separate registry.

```rust
use net_sdk::{tool, serve_tool, ToolEvent};

#[tool(
    id = "web_search",
    description = "Search the public web and return ranked results.",
    input_schema = web_search_input_schema(),
)]
async fn web_search(args: WebSearchArgs) -> Result<WebSearchReply, RpcError> {
    let hits = run_query(&args.query).await?;
    Ok(WebSearchReply { hits })
}

let handle = serve_tool(&mesh, web_search).await?;
```

The `serve_tool` registration is atomic: the handler, the capability-fold publish, the `nrpc:web_search` channel tag, the `ai-tool:web_search` discovery tag, and the auto-installed `tool.metadata.fetch` RPC for fetching oversized JSON Schemas all succeed together or none do. Dropping the handle reverses them in the same way.

**Discovering tools.** `list_tools` walks the capability fold in-memory and returns `ToolDescriptor`s. There's no RPC fan-out, no central registry — the fold already aggregates every node's capabilities, so tool discovery just reads it:

```rust
use net_sdk::list_tools;

let tools = list_tools(&mesh, matcher::any()).await?;
for tool in tools {
    println!("{} (v{}) — {} nodes", tool.id, tool.version, tool.node_count);
}
```

`watch_tools` is the streaming sibling — it emits `ToolListChange` events as tools appear, disappear, or change. Subnet visibility, capability auth, and region filtering all inherit from the existing fold and `TagMatcher` plumbing.

**Calling a tool.**

```rust
use net_sdk::call_tool;

let reply: WebSearchReply = call_tool(&mesh, "web_search", args).await?;
```

For streaming tools — model token generation, file download with progress, log tails — the handler returns a `Stream<ToolEvent>`:

```rust
use futures::stream;

#[tool(id = "summarize", streaming = true, ...)]
async fn summarize(args: SummarizeArgs) -> impl Stream<Item = ToolEvent> {
    stream::unfold(state, |s| async move {
        let chunk = generate_next_token(&s).await?;
        Some((ToolEvent::delta(chunk), s.advance()))
    })
}

let mut stream = call_tool_streaming(&mesh, "summarize", args).await?;
while let Some(event) = stream.next().await {
    match event? {
        ToolEvent::Delta { data }   => print!("{}", data),
        ToolEvent::Progress { pct, .. } => update_spinner(pct),
        ToolEvent::Result { data }  => break,
        ToolEvent::Error { code, message, .. } => eprintln!("{code}: {message}"),
        ToolEvent::Start { .. }     => {}
    }
}
```

The `ToolEvent` envelope is a tagged enum every streaming handler emits per chunk: `start` (fires once on open), `progress` (coarse progress for spinners), `delta` (partial output), `result` (terminal success), `error` (terminal failure). Unary tools synthesize a single `result` envelope under the hood. The convention lets every adapter (OpenAI / Anthropic / Gemini / MCP / your own) lower envelopes into the framework's native streaming protocol without per-pair negotiation.

**Format translators.** `@net-mesh/tools` (npm), `net-mesh-tools` (pip), and the equivalent Rust and Go packages ship pure-function translators from `ToolDescriptor` to each major provider's tool schema:

```ts
import { toOpenAITool, toAnthropicTool, toGeminiTool, toMCPTool } from "@net-mesh/tools";

const tools = await listTools(mesh);
const openaiTools = tools.map(toOpenAITool);
// Hand `openaiTools` to your OpenAI client; when the model emits a tool_use,
// pipe it back through lowerToolCall(call) to get a typed nRPC call spec.
```

No transitive dependency on any provider SDK — translators emit plain JSON, the user wires the output into their own model client. Cross-language byte-equality is pinned by golden vectors in CI, so a Python translator's output for a given descriptor matches the equivalent Rust / Node / Go translator's output to the byte.

## When to use nRPC vs. the bus

nRPC and the event bus solve different shapes. Reach for nRPC when the natural unit of work is a synchronous call with an answer the caller will wait for. Reach for the bus when the natural unit of work is a stream the producer doesn't track. Both share the underlying transport, identity, and authorization, so a system can — and usually does — use both.

A typical service uses the bus for telemetry (no caller to answer to), nRPC for control-plane operations (the caller wants to know whether the command worked), and a CortEX fold for the state that ties the two together (the call writes an event, the fold materializes a queryable view of the result). All three speak the same underlying primitives.
