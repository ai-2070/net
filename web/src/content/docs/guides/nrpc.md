# Typed RPC with nRPC

nRPC layers request/response semantics on top of channels. Where the bus is fire-and-forget — you publish, subscribers pull — nRPC gives you a typed call that takes arguments, returns a result, and threads identity, capability targeting, cancellation, and tracing through the same primitive.

You use nRPC when the natural shape of an interaction is a call: a client asks a service for something, the service computes, the client gets an answer. You stay on the event bus when the natural shape is a stream that producer and consumer don't synchronize on. Both are first-class; they share the same routing, the same identity, and the same encryption.

## The shape of a call

Define a service as a typed request and response plus a string name that identifies the method on the mesh. The types only need `serde` — the SDK's typed layer handles the wire encoding (JSON by default, selected per call via a `Codec`), the dispatch glue, and the request/response correlation:

```rust
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct EchoArgs {
    message: String,
}

#[derive(Serialize, Deserialize)]
struct EchoReply {
    echoed: String,
    received_at: u64,
}
```

The service name (`"demo.echo"`) is a plain string that uniquely identifies the method on the mesh. Names use the same hierarchical convention as channels — `module.method` is conventional, but the grammar is yours.

## Serving and calling — unary

A server registers a handler against the service name. The substrate pulls requests off the underlying channel, deserializes the args, runs the handler, and publishes the response back to the caller's reply channel. The handler returns `Ok(reply)` on success or `Err(message)` to fail the call:

```rust
use net_sdk::mesh_rpc::Codec;

let handle = mesh.serve_rpc_typed(
    "demo.echo",
    Codec::Json,
    |args: EchoArgs| async move {
        Ok::<_, String>(EchoReply {
            echoed: args.message,
            received_at: now_unix_ms(),
        })
    },
)?;
```

`serve_rpc_typed` is synchronous — it returns a `ServeHandle` immediately; dropping the handle unregisters the service. A client calls the service by name:

```rust
use net_sdk::mesh_rpc::CallOptionsTyped;

let reply: EchoReply = mesh
    .call_service_typed(
        "demo.echo",
        &EchoArgs { message: "hello".into() },
        CallOptionsTyped::default(),
    )
    .await?;
```

`call_service_typed` consults the local capability index for nodes advertising the service and routes to one. To address a specific node instead, use `call_typed(target_node_id, service, &request, opts)` — same typed shape, no index lookup.

If no node advertises the service, the call fails with `RpcError::NoRoute`. When several nodes serve it, `call_service_typed` picks one per the call's `RoutingPolicy` (round-robin by default), skipping candidates the proximity graph reports unhealthy. A handler that returns `Err(message)` comes back to the caller as `RpcError::ServerError { status, message, .. }` with `status == NRPC_TYPED_HANDLER_ERROR` (`0x8001`); a request body the handler can't deserialize surfaces the same way with `NRPC_TYPED_BAD_REQUEST` (`0x8000`), so callers can route validation failures and handler failures to different fall-back paths. Panics inside a handler are caught, counted on the server's `ServiceMetrics::handler_panics_total`, and surfaced to the caller as a server error rather than crashing the node.

## Streaming — server-streaming, client-streaming, duplex

Four call shapes share one wire and one typed surface. The unary case above is the simplest; the three streaming shapes layer on the same primitive.

**Server-streaming.** The handler pushes chunks into a typed `ResponseSinkTyped<Resp>` instead of returning a single value, then returns `Ok(())` to close the stream cleanly (or `Err(message)` to fail it). Useful for model inference with token streaming, file downloads with progress, or any case where one response would be too coarse:

```rust
use net_sdk::mesh_rpc::{CallOptionsTyped, Codec, ResponseSinkTyped};
use futures::StreamExt;

let handle = mesh.serve_rpc_streaming_typed(
    "tokens.generate",
    Codec::Json,
    |args: TokenArgs, sink: ResponseSinkTyped<String>| async move {
        for token in args.tokens {
            sink.send(&token)?; // push one chunk; Err(String) aborts the stream
        }
        Ok(())
    },
)?;

let mut stream = mesh
    .call_streaming_typed::<TokenArgs, String>(
        provider_node_id,
        "tokens.generate",
        &args,
        CallOptionsTyped::default(),
    )
    .await?;
while let Some(token) = stream.next().await {
    print!("{}", token?);
}
```

**Client-streaming.** The caller sends a sequence of request chunks; the server returns one terminal response after the stream closes:

```rust
let mut call = mesh
    .call_client_stream_typed::<Chunk, UploadReceipt>(
        provider_node_id,
        "upload",
        CallOptionsTyped::default(),
    )
    .await?;
for chunk in file_chunks {
    call.send(&chunk).await?;
}
let receipt: UploadReceipt = call.finish().await?;
```

The server-side handler receives a typed `RequestStreamTyped<Req>` that decodes each inbound chunk; a chunk that fails to decode terminates the stream with an `RpcError::Codec` item, and the handler returns one terminal `Resp` (or `Err(message)`) once the stream closes.

**Duplex.** Both sides send sequences of typed messages independently. Most useful for interactive sessions and long-running coordination:

```rust
let call = mesh
    .call_duplex_typed::<ChatMsg, ChatMsg>(
        provider_node_id,
        "chat",
        CallOptionsTyped::default(),
    )
    .await?;
let (mut sink, mut stream) = call.into_split();

tokio::spawn(async move {
    for msg in outgoing {
        sink.send(&msg).await.ok();
    }
    sink.finish_sending().await.ok();
});

while let Some(incoming) = stream.next().await {
    handle(incoming?);
}
```

All four shapes ship across Rust, Node, Python (sync + async), and Go with the same typed wrappers and the same wire contract. A Python client can drive a Go duplex handler that talks to a Node server-streaming handler — cross-language interop is pinned by shared golden vectors in CI.

## Capability-targeted calls

The interesting cases come when you don't just want *any* server — you want a server that matches some capability. nRPC carries a serialized predicate alongside the call. Servers that opt into predicate-pushdown evaluate it against their local capability set; a mismatched server refuses the call without invoking the handler:

```rust
use net_sdk::capabilities::pred;
use net_sdk::mesh_rpc::{CallOptionsExt, CallOptionsTyped};

let predicate = pred!(and [
    pred!(exists "hardware.gpu"),
    pred!(semver_at_least "software.cuda", "12.0"),
]);

let opts = CallOptionsTyped::default().with_where(&predicate)?;

let reply: InferenceReply = mesh
    .call_service_typed(
        "inference.run",
        &InferenceArgs { prompt: "…".into() },
        opts,
    )
    .await?;
```

The predicate ships in a `net-where:` request header. The receiver decodes it via `RpcContextExt::where_predicate()` and evaluates it against its own capability set; if it matches, the handler runs; if not, the call is refused. A call that matches no serving node fails with `RpcError::NoRoute`. This is how you build a service mesh without standing up a separate one — "route this call to a node with these capabilities" is a primitive, with no sidecar, no service-discovery layer, and no separate authentication step.

## Cancellation

Cancellation is a first-class substrate primitive. Reserve a token from the node handle, pair it with a call via `CallOptions::cancel_token`, and `cancel(token)` from any thread aborts the in-flight call cleanly — surfacing `RpcError::Cancelled` to the caller and emitting a CANCEL on the wire. Honored uniformly by every call shape (unary, server-streaming, client-streaming, duplex):

```rust
use net_sdk::mesh_rpc::{CallOptions, CallOptionsTyped};

// `mesh` is shared as an `Arc<Mesh>`. Cancel tokens live on the
// underlying node handle.
let node = mesh.node_arc();
let token = node.reserve_cancel_token();

let call = tokio::spawn({
    let mesh = mesh.clone();
    async move {
        mesh.call_typed::<ExpensiveArgs, ExpensiveReply>(
            provider_node_id,
            "expensive.op",
            &args,
            CallOptionsTyped {
                raw: CallOptions {
                    cancel_token: Some(token),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await
    }
});

// Some time later, from anywhere:
node.cancel(token);
```

The substrate registers the token's abort handle at construction and removes it on resolution. A cancel that arrives *before* the call's abort handle is registered (the gap between `reserve_cancel_token` and call construction) is held as a latched flag on an orphan entry; when the call later registers, it observes the flag and short-circuits to `RpcError::Cancelled` without ever publishing the REQUEST. Token reservations that never get used age out on an orphan TTL.

Idiomatic surfaces wrap the primitive in each binding:

- **Node:** `AbortSignal` end-to-end. Pass `signal` in the call options; `signal.abort()` cancels.
- **Python:** a cancel handle you pass via `cancel=` and trip with `.cancel()`.
- **Go:** `context.Context`. Pass `ctx` to the call; `cancel()` cancels.

All three lower to the same substrate token, so a TS client cancelling a call to a Python server is wire-equivalent to a Python client cancelling a call to a Go server. Power users can reserve tokens directly via the raw substrate surface for cross-call cancel sharing.

## Observers and metrics

The mesh exposes an observer hook and a per-service metrics snapshot. The observer fires per call with a typed `RpcCallEvent`; the snapshot reports cumulative counters for every service the node has called or served:

```rust
use std::sync::Arc;
use net_sdk::mesh_rpc::{RpcCallEvent, RpcObserver};

struct MetricsSink;

impl RpcObserver for MetricsSink {
    fn on_call(&self, evt: RpcCallEvent) {
        // evt.method, evt.latency_ms, evt.status, evt.request_bytes, …
        record(&evt.method, evt.latency_ms, &evt.status);
    }
}

mesh.set_rpc_observer(Some(Arc::new(MetricsSink)));

let snap = mesh.rpc_metrics_snapshot();
for svc in &snap.services {
    println!("{}: {} calls", svc.service, svc.calls_total);
}
```

`RpcCallEvent` carries the method name, the caller/callee node ids, request/response byte counts, latency in milliseconds, a `direction` (`Outbound` / `Inbound` — v1 fires `Outbound` only), and a `status` tagged enum (`Ok` / `Error(message)` / `Timeout` / `Canceled`). The observer swap is atomic mid-call, so a Prometheus exporter or a structured-log sink can be installed and torn down without disrupting in-flight work.

Critically, the substrate calls `on_call` **inline on the dispatch thread**, so a native Rust observer must itself be cheap — push into your own bounded channel or lock-free ring, don't block. Every language binding ships a ready-made trampoline (`ObserverChannel`) that does exactly this: each event is `try_send`'d onto a 1024-slot bounded mpsc drained by a worker task, so a slow callback — a disk-flushing logger, a network-pinning metrics exporter — can't pin the dispatch thread. When the buffer is full the event is dropped and a process-global `observer_dropped_total` counter increments, so dashboards can detect observer back-pressure.

The same shape exists in every binding: `setObserver` / `set_observer` / `SetObserver` plus `metricsSnapshot` / `metrics_snapshot` / `MetricsSnapshot`. Go consumers additionally get a `net_rpc_observer_dropped_total() -> u64` FFI symbol (and an `observer_dropped_total` field on the JSON snapshot) for monitoring without paying the decode cost on the snapshot path.

## Resilience

`call_typed` makes exactly one attempt and surfaces a typed error for everything else. When you want automatic re-issue on transient failures, the SDK ships a resilience layer that wraps the base call with bounded retries and exponential backoff. Build a `RetryPolicy` from its defaults, override the fields you care about, and call `call_typed_with_retry`:

```rust
use std::time::{Duration, Instant};
use net_sdk::mesh_rpc::{CallOptions, CallOptionsTyped};
use net_sdk::mesh_rpc_resilience::RetryPolicy;

let policy = RetryPolicy {
    max_attempts: 3,
    initial_backoff: Duration::from_millis(50),
    ..RetryPolicy::default()
};

let opts = CallOptionsTyped {
    raw: CallOptions {
        deadline: Some(Instant::now() + Duration::from_secs(5)),
        ..Default::default()
    },
    ..Default::default()
};

let reply: EchoReply = mesh
    .call_typed_with_retry::<EchoArgs, EchoReply>(
        provider_node_id,
        "demo.echo",
        &args,
        opts,
        &policy,
    )
    .await?;
```

By default the policy retries timeouts, transport failures, and transient server errors (internal / backpressure); it leaves `NoRoute`, codec, capability-denied, cancellation, and application handler errors alone — retrying a `bad request` forever is not recovery. Override the classifier with `RetryPolicy::with_retryable`. The deadline lives on `opts.raw.deadline` (an absolute `Instant`) and does *not* advance across retries, so the total wall-clock window is bounded by the initial deadline plus the sum of backoffs. Sibling helpers — `call_service_typed_with_hedge` (race a backup provider) and `CircuitBreaker` (fast-fail a sick target) — compose the same way; see [Recover a Failed Workflow](/docs/guides/recover-failed-workflow).

## AI tool calling

Every typed nRPC service can also expose itself as an LLM-callable tool. A tool registered as `web_search` IS the nRPC service at `web_search` IS the announcement carrying the `ai-tool:web_search` and `nrpc:web_search` capability tags — one identifier, one source of truth, no separate registry.

```rust
use net_sdk::macros::tool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(JsonSchema, Deserialize, Serialize)]
struct WebSearchArgs { query: String }

#[derive(JsonSchema, Deserialize, Serialize)]
struct WebSearchReply { hits: Vec<String> }

#[tool(
    name = "web_search",
    description = "Search the public web and return ranked results.",
    tag = "web",
)]
async fn web_search(args: WebSearchArgs) -> Result<WebSearchReply, String> {
    let hits = run_query(&args.query).await.map_err(|e| e.to_string())?;
    Ok(WebSearchReply { hits })
}

// The macro derives the JSON Schema from `WebSearchArgs`/`WebSearchReply`
// and generates `web_search_descriptor()` + `web_search_register(mesh)`
// alongside the function. Register on a live mesh:
let handle = web_search_register(&mesh)?;
```

The `serve_tool` registration behind `web_search_register` is atomic: the descriptor insert into the capability fold, the `nrpc:web_search` service registration, the `ai-tool:web_search` discovery tag, and the lazily auto-installed `tool.metadata.fetch` RPC for fetching oversized JSON Schemas all succeed together or none do. Dropping the returned handle reverses the descriptor insert and the handler registration in the same way.

**Discovering tools.** `list_tools` walks the capability fold in-memory and returns `ToolDescriptor`s. There's no RPC fan-out, no central registry — the fold already aggregates every node's capabilities, so tool discovery just reads it:

```rust
let tools = mesh.list_tools(None); // None = every tool the local fold has seen
for tool in tools {
    println!("{} (v{}) — {} nodes", tool.tool_id, tool.version, tool.node_count);
}
```

`list_tools` is synchronous and takes an `Option<&TagMatcher>` (pass `None` for "any", or a matcher to scope by tag / region / subnet). `watch_tools` is the streaming sibling — it emits `ToolListChange` events as tools appear, disappear, or change publisher count. Subnet visibility, capability auth, and region filtering all inherit from the existing fold and `TagMatcher` plumbing.

**Calling a tool.**

```rust
let reply: WebSearchReply = mesh.call_tool("web_search", &args).await?;
```

For streaming tools — model token generation, file download with progress, log tails — serve the tool through `serve_tool_streaming`, whose handler returns a `Stream<Item = ToolEvent>`:

```rust
use net_sdk::tool::{metadata_for, ToolEvent};
use futures::stream;

// `SummarizeArgs` / `SummarizeReply` derive `JsonSchema` + serde, as above.
let descriptor = metadata_for::<SummarizeArgs, SummarizeReply>("summarize")
    .description("Summarize text, streaming tokens as they generate.")
    .streaming(true)
    .build();

let handle = mesh.serve_tool_streaming(descriptor, |args: SummarizeArgs| async move {
    stream::unfold(initial_state(args), |state| async move {
        let (token, next) = generate_next_token(state).await?;
        let event = ToolEvent::Delta { data: serde_json::json!({ "token": token }) };
        Some((event, next))
    })
})?;
```

A streaming handler must emit exactly one terminal `ToolEvent::Result` or `ToolEvent::Error`; if it ends without one, the SDK synthesizes `ToolEvent::Error { code: "missing_terminal", .. }` so callers can rely on every stream ending with a terminal envelope. On the caller side, `call_tool_streaming` yields the envelopes as they arrive:

```rust
use futures::StreamExt;

let mut stream = mesh.call_tool_streaming("summarize", &args).await?;
while let Some(event) = stream.next().await {
    match event? {
        ToolEvent::Delta { data }              => print!("{data}"),
        ToolEvent::Progress { pct, .. }        => update_spinner(pct),
        ToolEvent::Result { data }             => { let _ = data; break }
        ToolEvent::Error { code, message, .. } => eprintln!("{code}: {message}"),
        ToolEvent::Start { .. }                => {}
    }
}
```

The `ToolEvent` envelope is a tagged enum every streaming handler emits per chunk: `start` (fires once on open), `progress` (coarse progress for spinners), `delta` (partial output), `result` (terminal success), `error` (terminal failure). Unary tools synthesize a single `result` envelope under the hood. The convention lets every adapter (OpenAI / Anthropic / Gemini / MCP / your own) lower envelopes into the framework's native streaming protocol without per-pair negotiation.

**Format translators.** The `tool` module in every binding — `net_sdk::tool::formats` (Rust), `@net-mesh/core/tool` (Node), `net.tool` (Python), plus the Go equivalent — ships pure-function translators from `ToolDescriptor` to each major provider's tool schema:

```ts
import { openai, listTools } from "@net-mesh/core/tool";

const tools = listTools(mesh);                     // sync — reads the local fold
const openaiTools = tools.map(openai.toOpenaiTool);
// Hand `openaiTools` to your OpenAI client; when the model emits a
// tool call, pipe it back through `openai.lowerOpenaiToolCall(call)`
// to get a typed spec for `mesh.callTool(spec.name, args)`.
```

No transitive dependency on any provider SDK — translators emit plain JSON, the user wires the output into their own model client. Cross-language byte-equality is pinned by golden vectors in CI, so a Python translator's output for a given descriptor matches the equivalent Rust / Node / Go translator's output to the byte.

## When to use nRPC vs. the bus

nRPC and the event bus solve different shapes. Reach for nRPC when the natural unit of work is a synchronous call with an answer the caller will wait for. Reach for the bus when the natural unit of work is a stream the producer doesn't track. Both share the underlying transport, identity, and authorization, so a system can — and usually does — use both.

A typical service uses the bus for telemetry (no caller to answer to), nRPC for control-plane operations (the caller wants to know whether the command worked), and a CortEX fold for the state that ties the two together (the call writes an event, the fold materializes a queryable view of the result). All three speak the same underlying primitives.
