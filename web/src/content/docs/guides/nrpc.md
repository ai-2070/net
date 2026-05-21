# Typed RPC with nRPC

nRPC layers request/response semantics on top of channels. Where the bus is fire-and-forget — you publish, subscribers pull — nRPC gives you a typed call that takes arguments, returns a result, and threads identity, capability targeting, and tracing through the same primitive.

You use nRPC when the natural shape of an interaction is a call: a client asks a service for something, the service computes, the client gets an answer. You stay on the event bus when the natural shape is a stream: a producer emits, consumers pull on their own schedule. Both are first-class; they share the same routing, the same identity, and the same encryption.

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

## Serving

A server registers a handler against the service name and starts a worker. The worker pulls requests off the underlying channel, deserializes the args, runs the handler, and publishes the response back to the caller's reply channel:

```rust
use net_sdk::serve_rpc;

let handle = serve_rpc(&mesh, EchoService, |args: EchoArgs| async move {
    Ok(EchoReply {
        echoed: args.message,
        received_at: now_unix_ms(),
    })
}).await?;

// `handle` stays alive as long as the service is serving.
// Drop it (or call `handle.shutdown()`) to stop accepting calls.
```

The handler is an async function from `EchoArgs` to `Result<EchoReply, RpcError>`. Failures returned from the handler are routed back to the caller as typed errors; panics are caught and reported as `RpcError::Panic`.

## Calling

A client calls the service by name. The SDK handles routing — the call goes to whichever server is currently advertising `demo.echo` and matches the (optional) capability filter:

```rust
use net_sdk::call_typed;

let reply: EchoReply = call_typed(
    &mesh,
    EchoService,
    EchoArgs { message: "hello".into() },
).await?;

println!("echoed: {}, at: {}", reply.echoed, reply.received_at);
```

If there's no server registered for the service, the call fails with `RpcError::NoServer`. If multiple servers are registered, the call goes to one of them — the SDK picks by load and proximity, biased toward the nearest healthy advertiser.

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

The predicate ships in a `net-where:` request header. The receiver's predicate evaluator runs against its own capability set; if it matches, the handler runs; if not, the call is routed elsewhere or fails with `RpcError::NoMatchingServer`.

This is how you build a service mesh without standing up a separate one. "Route this call to a node with these capabilities" is a primitive — no sidecar, no service-discovery layer, no separate authentication step.

## Streaming responses

For long-running calls — model inference with token streaming, file uploads with progress, anything where a single response would be too coarse — nRPC supports streaming responses. The handler returns a `Stream` instead of a single value:

```rust
use futures::Stream;

let stream = stream_rpc(&mesh, StreamingService, |args| async move {
    Ok(futures::stream::iter(args.tokens.into_iter().map(Ok)))
}).await?;

while let Some(token) = stream.next().await {
    print!("{}", token?);
}
```

Streaming is built on the same primitive — a streaming response is a sequence of events on a reply channel, terminated by an explicit end-of-stream marker. The framing is automatic; you write the handler against a `Stream`, the client reads against a `Stream`, and the wire details are the SDK's problem.

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

## Cross-binding interop

The wire format is the same across every binding. A Rust server can be called from TS, Python, or Go (and vice versa); the request/response types are serialized in a canonical form (`postcard` for Rust, JSON for the dynamic bindings, with explicit compatibility shims at the boundary) and tested against shared golden vectors in CI.

```python
# Python client calling a Rust server
from net import call_typed

reply = await call_typed(mesh, "demo.echo", {"message": "hello"})
print(reply["echoed"])
```

The cross-language story is intentionally minimal — every binding can call every service, with the SDK taking responsibility for the canonical encoding. You don't write IDL files; the request/response types are defined in the language they're served from, and the bindings consume them as ordinary records.

## When to use nRPC vs. the bus

nRPC and the event bus solve different shapes. Reach for nRPC when the natural unit of work is a synchronous call with an answer the caller will wait for. Reach for the bus when the natural unit of work is a stream the producer doesn't track. Both share the underlying transport, identity, and authorization, so a system can — and usually does — use both.

A typical service uses the bus for telemetry (no caller to answer to), nRPC for control-plane operations (the caller wants to know whether the command worked), and a CortEX fold for the state that ties the two together (the call writes an event, the fold materializes a queryable view of the result). All three speak the same underlying primitives.
