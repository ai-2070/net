# nRPC bidirectional streaming plan — client streaming, server-streaming for client-streamed requests, and full duplex

Spec for the three missing nRPC streaming shapes. Reference:
- `net/crates/net/docs/misc/NRPC_DESIGN.md` (architectural framing + Phase 3 server-streaming details)
- `net/crates/net/sdk/src/mesh_rpc.rs` (the SDK surface that ships server-streaming today)
- `net/crates/net/src/adapter/net/cortex/rpc.rs` (codec, wire format, server/client folds)

## Status

- ✅ **Unary** `Mesh::call_typed` / `serve_rpc_typed` — one Req, one Resp.
- ✅ **Server-streaming** `Mesh::call_streaming_typed` / `serve_rpc_streaming_typed` — one Req, many Resp. Window-grant flow control wired (`DISPATCH_RPC_STREAM_GRANT`, header `nrpc-stream-window-initial`).
- ✅ **Client-streaming** `Mesh::call_client_stream_typed` / `serve_rpc_client_stream_typed` — many Req, one Resp. Wire format: `DISPATCH_RPC_REQUEST_CHUNK` + `FLAG_RPC_CLIENT_STREAMING_REQUEST` + `FLAG_RPC_REQUEST_END`. Request-direction flow control via `DISPATCH_RPC_REQUEST_GRANT` + header `nrpc-request-window-initial`. Substrate `ClientStreamCallRaw` and SDK veneer `ClientStreamCallTyped<Req, Resp>`.
- ✅ **Duplex** `Mesh::call_duplex_typed` / `serve_rpc_duplex_typed` — many Req ↔ many Resp interleaved. Reuses both `FLAG_RPC_CLIENT_STREAMING_REQUEST` (request side) and `FLAG_RPC_STREAMING_RESPONSE` (response side) on the initial REQUEST. Substrate `DuplexCallRaw` (with `into_split` → `DuplexSink` + `DuplexStream`), SDK veneer `DuplexCallTyped<Req, Resp>` plus typed `DuplexSinkTyped<Req>` / `DuplexStreamTyped<Resp>`. Shared `Arc<DuplexInner>` so CANCEL fires only when both halves drop without clean close.
- ✅ **Server-side handler for client-streamed requests** — `RpcClientStreamingHandler` async-trait taking `(ctx: RpcStreamingContext, requests: RequestStream) -> Result<RpcResponsePayload, RpcHandlerError>`. `RpcDuplexHandler` adds the response sink. Both implemented by `RpcStreamingRequestFold` / `RpcDuplexFold` in `cortex::rpc`. SDK veneer adds `RequestStreamTyped<Req>` (flattened) and the opt-in `ChunkedRequestStream<Req>` via `into_chunked()` for callers that need to distinguish `Chunk::Init` from `Chunk::Data`.

### Delivered commits on `nrpc-streaming` (branched off master after the `nrpc-benchmarks` PR merged)

| Commit | Phase | Notes |
|---|---|---|
| `1a35864d` | A — wire format | 7 new public items in `cortex/rpc.rs`, 6 codec / pin tests |
| `20a94366` | B — server fold | `RpcStreamingRequestFold` + `RpcClientStreamingHandler` + `RequestStream` + 6 unit tests |
| `bb53262e` | C-substrate | `PendingEntry::ClientStreaming`, `RpcClientPending::register_client_streaming`, `deliver_grant`, REQUEST_GRANT routing on `RpcClientFold` + 6 unit tests |
| `e051fbe3` | C-glue | `MeshNode::serve_rpc_client_stream` + `MeshNode::call_client_stream` + `ClientStreamCallRaw` |
| `f8f054bd` | C-tests | 5 real-network integration tests + the empty-body-on-`FLAG_END` terminator-semantics fix |
| `60446024` | D — substrate + glue | `RpcDuplexHandler`, `RpcDuplexFold`, `PendingEntry::Duplex`, `MeshNode::serve_rpc_duplex` / `call_duplex`, `DuplexCallRaw` / `DuplexSink` / `DuplexStream`, shared `Arc<DuplexInner>` CANCEL semantics |
| `458cdb27` | D-tests | 5 real-network duplex integration tests |
| `e70b93e8` | E — SDK veneer | All four `*_typed` methods on `Mesh`, `Chunk<T>`, `RequestStreamTyped` + `ChunkedRequestStream`, `ClientStreamCallTyped`, `DuplexCallTyped` + split halves, 5 SDK-level tests |
| `ecbafbc6` | F — benches | `nrpc_client_streaming` + `nrpc_duplex` Criterion benches extending the existing nRPC suite |

Test totals on the branch: **23 new unit tests + 10 real-network integration tests + 5 SDK-level typed tests = 38 added**, all green. Existing typed tests (`mesh_rpc_typed.rs`, `mesh_rpc_streaming_typed.rs`) regression-swept post-Phase-E: 7/7 still pass — Phase E only ADDED methods, didn't modify existing ones.

Phases G (Node / Python / Go binding parity) remains deferred per this plan's original scope; it gets its own follow-up plan once the wire contract has had real cross-binding usage.

Existing Phase 3 prerequisites that this plan composes against (no rework needed):

- `RpcResponseSink` + `ResponseSinkTyped<Resp>` — the response sink we'll reuse for the duplex server side.
- `RpcStream` + `RpcStreamTyped<Resp>` — the caller-side response stream we'll reuse for the duplex caller side.
- `RpcServerStreamingFold` — already streams Resp chunks; will be generalized to also accept Req chunks.
- Flow-control primitives (`Semaphore`-backed `FlowControlMap`, `DISPATCH_RPC_STREAM_GRANT`) — mirror to the request direction.
- W3C trace context propagation, RPC metrics registry, retry/hedge wrappers — all caller-side primitives; will need streaming-aware mirrors only where the semantics differ (e.g. retry doesn't apply to a half-consumed upload stream).

## Goal & scope

Add three surfaces that together close the streaming matrix:

1. **`Mesh::call_client_stream_typed<Req, Resp>(target, service, opts) -> ClientStreamCall<Req, Resp>`** — caller-side primitive: push N `Req`s into a sink, then await one terminal `Resp`. Direct routing only; capability-based variant follows the existing `_service_` naming.
2. **`Mesh::serve_rpc_client_stream_typed<Req, Resp, F, Fut>(service, codec, handler)`** — server-side handler shape: receives a `RequestStreamTyped<Req>` (futures::Stream), returns one `Resp`. Mirrors the streaming-response handler shape.
3. **`Mesh::call_duplex_typed<Req, Resp>(target, service, opts) -> DuplexCall<Req, Resp>`** — full duplex: caller has both a `RequestSink<Req>` and a `RpcStreamTyped<Resp>`, can interleave freely.
4. **`Mesh::serve_rpc_duplex_typed<Req, Resp, F, Fut>(service, codec, handler)`** — server-side full duplex: handler receives `(RequestStreamTyped<Req>, ResponseSinkTyped<Resp>)`.

In every case the **rust-channel abstraction** (mpsc-style sender + receiver wrapped in `futures::Sink` / `futures::Stream`) is the application-facing API, NOT a literal `Mesh::publish` channel. The channel layer's pub/sub is a transport detail, same as it is today for server-streaming responses.

Out of scope:

- Schema-validated payloads / IDL codegen — deferred (matches NRPC_BINDINGS_PLAN.md stance).
- Non-typed raw-bytes variants for duplex / client-streaming — will exist as the substrate the typed wrappers compose on, but won't get first-class SDK ergonomics in v1.
- Cross-language binding support (Node / Python / Go) — separate plan; this plan ships Rust SDK + wire format only.

## Architecture: substrate + veneer

This plan splits cleanly into two layers, and the split is load-bearing — application authors should never see anything from the lower layer.

### Substrate (`adapter::net::cortex::rpc` + `adapter::net::mesh_rpc`)

The wire format, the dispatch IDs, the folds, the flow-control semaphores. Everything below.

| | |
|---|---|
| Dispatch IDs | `DISPATCH_RPC_REQUEST_CHUNK` (0x15), `DISPATCH_RPC_REQUEST_GRANT` (0x16) |
| Flag bits | `FLAG_RPC_CLIENT_STREAMING_REQUEST`, `FLAG_RPC_REQUEST_END` |
| Headers | `nrpc-request-window-initial` |
| Types | `RpcStreamingRequestFold`, `RequestStream` (yields `Result<Bytes>`), `RpcClientStreamingHandler`, `RpcDuplexHandler` |
| State | `RequestFlowControlMap` (semaphore-per-call), `PendingEntry::{ClientStreaming, Duplex}`, `RpcCancellationToken` |
| Lifecycle | `call_id` allocation, reply-channel subscription, CANCEL emission on drop |

Substrate code talks in `Bytes`, knows about flags, owns the semaphores. It exists to make the wire format work. Nothing here should leak into application code.

### Veneer (`net_sdk::mesh_rpc`)

Typed Rust APIs that wrap the substrate and present an ergonomic mental model.

```rust
// Client-streaming.
let mut call = mesh.call_client_stream_typed::<Req, Resp>(target, "service").await?;
call.send(&Req { ... }).await?;
call.send(&Req { ... }).await?;
let resp: Resp = call.finish().await?;

// Duplex.
let mut call = mesh.call_duplex_typed::<Req, Resp>(target, "chat").await?;
let (mut sink, mut stream) = call.into_split();
sink.send(&Req { message: "hi" }).await?;
while let Some(resp) = stream.next().await {
    println!("got: {:?}", resp?);
}

// Server handler.
mesh.serve_rpc_client_stream_typed("service", Codec::Json,
    |mut requests: RequestStreamTyped<Req>| async move {
        let mut acc = 0;
        while let Some(req) = requests.next().await {
            acc += req?.value;
        }
        Ok::<_, String>(Resp { total: acc })
    },
)?;
```

What user code does NOT see (and the veneer is responsible for keeping invisible):

- `DISPATCH_RPC_REQUEST_CHUNK` and other dispatch IDs.
- `FLAG_RPC_REQUEST_END` and other flag bits — `finish()` / `finish_sending()` set the flag.
- `Semaphore::acquire_owned()` — window grants flow through `grant_request_window(n)` if the user opts in, otherwise the SDK manages them.
- `RpcCancellationToken` — `Drop` on a `ClientStreamCall` / `DuplexCall` fires CANCEL automatically.
- `call_id` plumbing — exposed read-only via `call_id()` for diagnostics, but the SDK allocates and threads it.
- Reply-channel subscription — already lazy-cached today; veneer reuses the existing path.
- `PendingEntry::*` — internal to the client fold.
- Raw `Bytes` — user types are `Req` / `Resp`, decoded by the captured `Codec` (see `Chunk<T>` below).

### `Chunk<T>` — the SDK-internal frame type

The substrate yields raw `Bytes` per frame. The veneer needs to (a) decode bytes into `T`, (b) track which frames are "initial," "mid-stream," and "terminator," and (c) hide all that from user code that just wants a stream of `T`s.

The veneer's internal abstraction:

```rust
// SDK-internal. Not exposed in the default user API.
#[derive(Debug)]
pub(crate) enum Chunk<T> {
    /// Decoded from the initial REQUEST (FLAG_CLIENT_STREAMING set, FLAG_END unset).
    /// Carries optional caller-supplied session metadata.
    Init(T),
    /// Decoded from a non-terminal REQUEST_CHUNK.
    Data(T),
    /// REQUEST_CHUNK with FLAG_REQUEST_END set. Body, if any, may carry a final
    /// `T` for the "init + end in one frame" degenerate case.
    End(Option<T>),
}
```

Critical: **`Chunk<T>` is not a wire encoding.** The wire format remains the substrate's flag-bit-tagged frames. `Chunk<T>` is the SDK's internal representation that the veneer constructs from each incoming frame after the substrate hands it `Bytes` + the flag context. If `Chunk<T>` were serialized verbatim, every frame would pay a 1-byte enum-discriminator tax for zero benefit — the flag bits already encode the variant.

User-facing API for the typed request stream:

```rust
pub struct RequestStreamTyped<Req> { /* ... */ }

impl<Req: DeserializeOwned + Unpin> futures::Stream for RequestStreamTyped<Req> {
    type Item = Result<Req, RpcError>;
    // Flattened: Init(req) and Data(req) both yield Some(Ok(req)); End yields None.
    // Decode failure yields Some(Err(RpcError::Codec)) then None.
}

// Advanced opt-in for users who need to distinguish Init from Data
// (sessions with explicit init handshake, etc.):
impl<Req> RequestStreamTyped<Req> {
    pub fn into_chunked(self) -> ChunkedRequestStream<Req>;
}

pub struct ChunkedRequestStream<Req> { /* yields Stream<Item = Result<Chunk<Req>, RpcError>> */ }
```

The default flattened API is what 99% of users want. The `into_chunked()` escape hatch exists for the small set of callers that care about the Init-vs-Data distinction (e.g. "first frame carries the auth handshake, subsequent frames are payload").

### Layer boundaries

| Layer | Owns | Talks in | Can change without breaking | Cannot change without breaking |
|---|---|---|---|---|
| Substrate | Wire format, folds, dispatch IDs, semaphores | `Bytes`, dispatch IDs, flags | Internal fold structure, semaphore strategy, dispatch queue | Wire format (forward-compat or version bump required) |
| Veneer | Typed wrappers, `Chunk<T>` semantics, `ClientStreamCall` / `DuplexCall` shapes | `T`, `Codec`, futures Stream/Sink | `Chunk<T>` variants (additive), helper method names, error message text | Public method signatures, default-flattened stream semantics |

Substrate evolves as a wire-format / performance question. Veneer evolves as an API-ergonomics question. They version separately; veneer can iterate freely on ergonomics as long as the substrate contract is honored.

## Wire format additions

The existing streaming-response wire format (Phase 3) is the template. We mirror it to the request direction.

### New dispatch constants

```text
DISPATCH_RPC_REQUEST_CHUNK    = 0x15   // caller → server, multi-fire continuation of REQUEST
DISPATCH_RPC_REQUEST_GRANT    = 0x16   // server → caller, request-direction flow-control credit
```

`DISPATCH_RPC_REQUEST` (0x10, existing) keeps its role: opens the call. The new `REQUEST_CHUNK` events are tagged with the same `call_id` as the initial REQUEST and carry the same payload shape minus the redundant fields (no `service` repeat, no `deadline_ns` per chunk). Wire layout:

```text
RPC_REQUEST_CHUNK payload:
  call_id:        u64_le        (matches the original REQUEST)
  flags:          u16_le        (FLAG_RPC_REQUEST_END | FLAG_RPC_PROPAGATE_TRACE | ...)
  headers_count:  u8            (typically 0 — opaque per-chunk metadata stays optional)
  headers:        Vec<RpcHeader>
  body_len:       u32_le        (capped at MAX_RPC_BODY_LEN, same as REQUEST)
  body:           Vec<u8>
```

### New flag bits

```text
FLAG_RPC_CLIENT_STREAMING_REQUEST = 0x0010   // REQUEST opens a client-streaming or duplex call
FLAG_RPC_REQUEST_END              = 0x0020   // REQUEST_CHUNK is the terminal upload chunk
```

The initial REQUEST sets `FLAG_RPC_CLIENT_STREAMING_REQUEST` (caller signals "I'll push more chunks"); each `REQUEST_CHUNK` is non-terminal; the final upload is a `REQUEST_CHUNK` with `FLAG_RPC_REQUEST_END`. Caller may set `FLAG_RPC_REQUEST_END` on the initial REQUEST itself if it has zero additional chunks — useful for the degenerate "client-streaming with 1 item" case without a separate event.

For duplex, the caller sets **both** `FLAG_RPC_CLIENT_STREAMING_REQUEST` and `FLAG_RPC_STREAMING_RESPONSE` (existing) on the initial REQUEST. Server's RESPONSE chunks ride on the existing wire path (Phase 3), unchanged.

### Request-direction window grants

Mirror of the existing response-direction `DISPATCH_RPC_STREAM_GRANT` (0x14):

```text
RPC_REQUEST_GRANT payload:
  call_id:   u64_le
  credits:   u32_le
```

Caller sets `nrpc-request-window-initial` header on the initial REQUEST to ask for upload backpressure (analogous to `nrpc-stream-window-initial`). Server's fold maintains a per-call `Arc<Semaphore>` keyed on `(caller_origin, call_id)` in a new `RequestFlowControlMap`. Pump task `acquire_owned().await` + `forget()` per REQUEST_CHUNK delivered to the handler's stream; `add_permits(n)` on REQUEST_GRANT receipt; window collapses to 1 if header absent (preserves at-least-once delivery without forcing the caller to manage credits). Defensive `>>4` cap on GRANT amounts, same as response direction.

### Termination + cancel

- **Caller-initiated EOS**: `REQUEST_CHUNK` with `FLAG_RPC_REQUEST_END` set. Server's `RequestStream` yields `None` on receipt. Server handler then runs to completion and emits its terminal RESPONSE.
- **Server-initiated EOS**: terminal RESPONSE frame (today's `nrpc-streaming: end` header, or status≠Ok). Server's fold drops any in-flight REQUEST_CHUNKs after terminal RESPONSE.
- **CANCEL**: existing `DISPATCH_RPC_CANCEL` flips both directions. Server's `RequestStream` yields one final `Err(RpcError::Cancelled)` then closes; caller's `RpcStream` and `RequestSink` both close. No new wire constants.

### Back-compat

Pre-fix peers that don't understand `REQUEST_CHUNK` / `REQUEST_GRANT` simply receive them as unknown dispatch IDs and follow the existing `UnknownVersion` path (logged, continued, fold not killed — see `NRPC_DESIGN.md` Phase 1 commit). New clients calling old servers with `FLAG_RPC_CLIENT_STREAMING_REQUEST` will get an `RpcError::ServerError(UnknownVersion)` back if the server isn't streaming-aware; clients can fall back to unary or error cleanly.

## Server-side: `RpcStreamingRequestFold` + handler shape

New fold parallel to `RpcServerStreamingFold` but consuming REQUEST_CHUNK on the input side too.

### Internal structure

```rust
pub struct RpcStreamingRequestFold {
    // Per-call request-side state.
    in_flight: DashMap<(u64 /*caller_origin*/, u64 /*call_id*/), RequestInFlight>,
    request_flow_control: Arc<RequestFlowControlMap>,
    emitter: RpcAsyncResponseEmitter,
    handler: Arc<dyn RpcStreamingRequestHandler>,
}

struct RequestInFlight {
    sender: mpsc::Sender<Result<Bytes, RpcError>>,  // bounded by initial window
    deadline_ns: u64,
    trace_context: Option<TraceContext>,
    cancellation: Arc<RpcCancellationToken>,
}
```

On REQUEST receipt: allocate `RequestInFlight`, spawn the handler task, hand it the receiver wrapped in a `RequestStream`. On REQUEST_CHUNK: look up `(caller_origin, call_id)`, `acquire` a permit if flow-controlled, push body into the sender. On REQUEST_END (flag on the chunk): close the sender. On CANCEL: cancel the token + drop the entry. On terminal RESPONSE emitted by the handler: drop the entry (the existing response fold already does this).

### Handler traits

Two new async-traits parallel to `RpcHandler` / `RpcStreamingHandler`:

```rust
#[async_trait]
pub trait RpcClientStreamingHandler: Send + Sync + 'static {
    async fn call(
        &self,
        ctx: RpcStreamingContext,
        requests: RequestStream,
    ) -> Result<RpcResponsePayload, RpcHandlerError>;
}

#[async_trait]
pub trait RpcDuplexHandler: Send + Sync + 'static {
    async fn call(
        &self,
        ctx: RpcStreamingContext,
        requests: RequestStream,
        responses: RpcResponseSink,    // existing type, reused
    ) -> Result<(), RpcHandlerError>;
}
```

`RpcStreamingContext` is `RpcContext` minus the eager `payload` (because the first request is delivered through the stream, not pre-decoded). It carries `caller_origin`, `call_id`, `cancellation`, `trace_context`, and the request-direction `deadline_ns`. Headers from the initial REQUEST land here; per-chunk headers go on the `RequestStream::Item`.

`RequestStream` is `futures::Stream<Item = Result<Bytes, RpcError>>` — bytes-typed at the fold layer; SDK adds the typed wrapper.

### Panic / error semantics

Same as the existing folds:

- Handler returning `Err(RpcHandlerError::Application { code, message })` → terminal RESPONSE with `RpcStatus::Application(code)` + `message` body.
- Handler returning `Err(RpcHandlerError::Internal(msg))` → terminal `Internal`.
- Handler panic caught by `catch_unwind` → terminal `Internal` with panic body.
- For duplex specifically: any RESPONSE chunks already emitted by the handler stay; the panic produces an additional terminal frame.

## Caller-side: `ClientStreamCall<Req, Resp>` + `DuplexCall<Req, Resp>`

Mirror of the existing `RpcStream`. The caller gets a sink for outbound REQUEST_CHUNKs plus, for duplex, the existing inbound response stream.

### Surfaces

```rust
/// Caller side of a client-streaming call. Push N requests, then
/// `finish()` returns the terminal response. Once `finish()` is
/// called the sink is closed; subsequent `send`s return `Err`.
pub struct ClientStreamCall<Req, Resp> { ... }

impl<Req: Serialize, Resp: DeserializeOwned> ClientStreamCall<Req, Resp> {
    pub async fn send(&mut self, value: &Req) -> Result<(), RpcError>;
    pub async fn finish(self) -> Result<Resp, RpcError>;
    pub fn call_id(&self) -> u64;
    pub fn grant_request_window(&self, n: u32);  // explicit credit for cadence control
}

/// Caller side of a duplex call. Compose freely: `send` outbound
/// and `poll` inbound interleave. Both halves share the same
/// underlying call_id and CANCEL on drop.
pub struct DuplexCall<Req, Resp> { ... }

impl<Req: Serialize, Resp: DeserializeOwned> DuplexCall<Req, Resp> {
    pub async fn send(&mut self, value: &Req) -> Result<(), RpcError>;
    pub async fn finish_sending(&mut self);     // emits REQUEST_END, leaves response stream open
    pub fn into_split(self) -> (DuplexSink<Req>, DuplexStream<Resp>);
    pub fn call_id(&self) -> u64;
}

impl<Resp: DeserializeOwned + Unpin> futures::Stream for DuplexCall<...> {
    type Item = Result<Resp, RpcError>;
}
```

`into_split` is the bridge that lets application code park the sink in one task and the stream in another — the common "encoder task + decoder task" shape. Both halves carry an `Arc<UnaryCallGuard>` (the existing CANCEL-on-drop primitive) so dropping either does NOT cancel; both must drop for CANCEL to fire.

### Underlying mechanics

- One `RpcClientPending` slot per call, same as existing streaming-response. New `PendingEntry::ClientStreaming { resp_sender }` and `PendingEntry::Duplex { resp_sender, request_grant_sender }` variants on the existing enum.
- The caller's reply subscription (`<service>.replies.<caller_origin>`) is already lazy-cached per (target, service); no new subscription work.
- Outbound `REQUEST_CHUNK` events go through `publish_to_peer` direct-unicast, same as the initial REQUEST (asymmetric routing pattern from NRPC_DESIGN.md is preserved).

## Typed wrappers

Detailed in **Phase E** below. Briefly: `serve_rpc_client_stream_typed` / `serve_rpc_duplex_typed` / `call_client_stream_typed` / `call_duplex_typed` on `Mesh`, with `RequestStreamTyped<Req>` (flattened default) and `ChunkedRequestStream<Req>` (opt-in for Init-vs-Data discrimination). All four compose against the substrate's raw-bytes path; the raw path stays available for users who manage their own serialization (proto / postcard / hand-rolled). See the Substrate vs SDK boundary table for the layer contract.

## Resilience: which existing wrappers apply

| Wrapper | Client-streaming | Duplex |
|---|---|---|
| `call_with_retry` | ❌ (a half-consumed upload stream can't be replayed automatically; users opt into idempotency themselves) | ❌ (same reason × 2) |
| `call_with_hedge` | ❌ (hedge would duplicate every upload chunk; bandwidth waste, no win) | ❌ |
| `RpcMetricsRegistry` | ✅ (each call gets one `nrpc_calls_total` increment + latency from first REQUEST to terminal RESPONSE) | ✅ |
| W3C trace context | ✅ (header carried on initial REQUEST; same as today) | ✅ |
| `RpcObserver` | ✅ (one Outbound event on terminal completion) | ✅ |

Retry / hedge are intentionally not extended. Documented as a limitation — application code can wrap idempotent client-streaming calls in its own retry, but the SDK won't auto-replay because there's no general way to know if a partial upload corrupted server state.

## Phasing

Each phase is independently mergeable, with tests, and leaves the existing surfaces unchanged. **Phases A-D land in the substrate; Phase E lands in the veneer.** No phase below E is intended for application code to touch.

### Phase A — wire format (substrate)

- Add `DISPATCH_RPC_REQUEST_CHUNK` (0x15), `DISPATCH_RPC_REQUEST_GRANT` (0x16) constants.
- Add `FLAG_RPC_CLIENT_STREAMING_REQUEST` (0x0010), `FLAG_RPC_REQUEST_END` (0x0020).
- Add `nrpc-request-window-initial` header.
- Codec + decoder for `RpcRequestChunkPayload` and `RpcRequestGrantPayload`. `encode_request_chunk` / `decode_request_chunk` / `encode_request_grant` / `decode_request_grant` helpers.
- Test fixtures: 5 wire-stability tests on the new payloads (round-trip, truncation rejection, `body` cap, `headers` cap, malformed-flag rejection).

### Phase B — server fold for client-streaming (substrate)

- `RpcStreamingRequestFold` (parallel to `RpcServerStreamingFold`).
- `RpcClientStreamingHandler` trait + `RequestStream` type.
- `MeshNode::serve_rpc_client_stream(service, handler) -> ServeHandle`. Auto-registers `<service>.requests` exact-match + `<service>.replies.` prefix (same as today's `serve_rpc`).
- Cancellation: REQUEST_END closes the stream; CANCEL flips the token and surfaces one `Err(RpcError::Cancelled)` on the stream.
- Tests: 5 integration tests (`tests/integration_nrpc_client_streaming.rs`): basic round-trip with N=10 chunks, REQUEST_END semantics, CANCEL surfaces to handler, handler `Err` round-trips, handler panic surfaces as Internal.

### Phase C — caller side for client-streaming (substrate)

- `ClientStreamCall<Req, Resp>` opaque type with `send` / `finish` / `grant_request_window`.
- `MeshNode::call_client_stream(target, service, opts) -> Result<ClientStreamCallRaw, RpcError>` raw-bytes substrate.
- `PendingEntry::ClientStreaming` variant on `RpcClientPending`.
- Caller-side request-window grant: helper to emit `REQUEST_GRANT` events from the server fold when the handler's stream consumer is slower than the caller's send rate.
- Tests: 4 integration tests through real network: caller sends 10 → server collects 10 → caller sees terminal Resp; caller drops mid-send → server sees CANCEL; caller closes via `finish` with 0 sends → server's stream yields one None, handler returns terminal Resp; window-grant throttles caller send when server is slow.

### Phase D — duplex (substrate; composes B + C)

- `RpcDuplexHandler` trait. `RpcStreamingRequestFold` extended to thread the response sink through to the handler (one new variant, no new fold type).
- `DuplexCall<Req, Resp>` caller-side surface with `into_split`.
- `PendingEntry::Duplex` variant.
- Tests: 6 integration tests: bidirectional echo (caller sends 5 / server echoes each / caller sees 5 then EOF); interleaved send-while-receiving; server finishes first (caller's send after server EOF errors cleanly); caller finishes first (server can still emit responses); `into_split` lets sink + stream live in separate tasks; CANCEL from either side closes both halves.

### Phase E — SDK veneer (typed wrappers + Chunk<T>)

This is the layer application code actually sees. Substrate must already be in place (Phases A-D); this phase adds zero new wire bits and exists purely to make the substrate pleasant.

**Internal types**

- `Chunk<T>` enum (crate-private to `net_sdk::mesh_rpc`): `Init(T) | Data(T) | End(Option<T>)`. Constructed by the SDK from each substrate-yielded frame using the flag context (`FLAG_CLIENT_STREAMING_REQUEST`, `FLAG_REQUEST_END`) plus the bytes-decoded `T`.
- `Codec::decode_chunk<T>(bytes, flags) -> Result<Chunk<T>, RpcError>` helper that classifies the frame and decodes the body in one step.

**Public surfaces**

- `Mesh::serve_rpc_client_stream_typed<Req, Resp, F, Fut>(service, codec, handler)` where handler is `Fn(RequestStreamTyped<Req>) -> Future<Output = Result<Resp, String>>`.
- `Mesh::serve_rpc_duplex_typed<Req, Resp, F, Fut>(service, codec, handler)` where handler is `Fn(RequestStreamTyped<Req>, ResponseSinkTyped<Resp>) -> Future<Output = Result<(), String>>`.
- `Mesh::call_client_stream_typed<Req, Resp>(target, service, opts) -> Result<ClientStreamCall<Req, Resp>, RpcError>`.
- `Mesh::call_duplex_typed<Req, Resp>(target, service, opts) -> Result<DuplexCall<Req, Resp>, RpcError>`.

**Veneer types**

- `RequestStreamTyped<Req>: futures::Stream<Item = Result<Req, RpcError>>` — flattened default. Yields `Some(Ok(req))` for both Init and Data; yields `None` on End. Decode failure surfaces one `Err(RpcError::Codec)` then closes.
- `RequestStreamTyped::into_chunked() -> ChunkedRequestStream<Req>` — opt-in escape hatch yielding `Result<Chunk<Req>, RpcError>` for the rare caller that needs Init vs Data.
- `ClientStreamCall<Req, Resp>` — sink with `send(&Req)` / `finish() -> Resp` / `grant_request_window(n)` / `call_id()`. CANCEL on Drop.
- `DuplexCall<Req, Resp>` — combined surface with `send(&Req)` / `finish_sending()` (emits REQUEST_END, leaves response stream open) / `into_split() -> (DuplexSink, DuplexStream)` / `futures::Stream` impl yielding `Resp`. Shared `Arc<UnaryCallGuard>` between halves so CANCEL only fires when both drop.
- `DuplexSink<Req>: futures::Sink<Req>` and `DuplexStream<Resp>: futures::Stream<Item = Result<Resp, RpcError>>`.
- Re-export `ResponseSinkTyped<Resp>` (existing) so duplex handlers have the same surface as today's server-streaming handlers.

**No new wire bits.** Every frame the veneer emits and consumes is already defined by Phases A-D. The veneer's responsibility is encode-on-send / decode-on-poll, lifecycle wiring (CANCEL on Drop, FLAG_REQUEST_END on finish), and the `Chunk<T>` classification.

**Tests** (5 SDK tests, `sdk/tests/mesh_rpc_bidi_typed.rs`, all over real network handshake):

- `client_stream_typed_collects_all_chunks` — caller `send`s 10 typed requests, calls `finish`, server handler observes all 10 via the flattened stream, returns one Resp; round-trip pins.
- `client_stream_typed_handler_error_after_partial_stream` — handler returns `Err(String)` mid-stream, surfaces as `RpcError::ServerError(Application(0x4001))`.
- `client_stream_typed_decode_failure_terminates` — server sends a frame the caller's `Resp` can't decode; caller sees one `Err(RpcError::Codec)` then EOF, never silently swallows.
- `duplex_typed_interleaves_send_and_recv` — caller streams 5 `Req`s; server responds with one `Resp` per `Req` plus a final summary; caller observes all 6 responses in order while still sending.
- `duplex_typed_into_split_lets_halves_live_in_separate_tasks` — sink in one tokio task, stream in another, neither aborts the other; CANCEL only fires when both are dropped (pins the `Arc<UnaryCallGuard>` semantics).

**Doc comments + examples.** `nrpc_echo.rs` gets two new sibling examples: `nrpc_client_stream.rs` and `nrpc_duplex.rs`, demonstrating the veneer in ~50 lines each. The substrate-level types (`RpcStreamingRequestFold`, `Chunk<T>`, `RequestFlowControlMap`, etc.) are NOT re-exported from `net_sdk::mesh_rpc` — they live in `net::adapter::net::cortex::rpc` for users who need to compose against the raw substrate; surfacing them from the SDK would erode the layer boundary.

### Phase F — benchmarks (extends the suite landed in this branch; veneer)

- Add `nrpc_client_streaming.rs` to the bench suite. Same shape as `nrpc_streaming.rs` but flipped: caller pushes N chunks, server collects, caller awaits one terminal response. Axes: chunks per call (16, 256), payload per chunk (64 B, 1 KiB). Window-grant variant on / off.
- Add `nrpc_duplex.rs`. Caller and server each emit N items, measure end-to-end throughput + per-direction msgs/sec.
- These were already mentioned as out-of-scope in Phase 3 of NRPC_DESIGN.md; this plan finally closes them.

### Phase G — binding parity (deferred to a separate plan)

Once the Rust SDK ships, Node / Python / Go need parity surfaces. The bindings plan (`NRPC_BINDINGS_PLAN.md`) currently lists "Server-streaming + bidirectional streaming — only response-streaming is exposed" as out of scope; that line gets revised once Phase E lands and the wire contract is locked.

## Risks + open questions

1. **Memory growth on slow consumers** — a duplex handler that buffers responses faster than the caller drains them can hold unbounded memory. The existing response-direction `nrpc-stream-window-initial` is a per-call cap; the new request-direction mirror gives the same property to uploads. Default both windows to `64` chunks if the caller doesn't specify (gives reasonable batching without unbounded buffering). To decide: should the default be configurable on `MeshBuilder` so operators can tune the floor?

2. **Termination ordering for duplex** — what if the server emits its terminal RESPONSE before the caller has sent its last chunk? Today: server's RESPONSE is authoritative (call ends; in-flight REQUEST_CHUNKs are dropped). Document as the contract; if applications want "server reads all my upload before answering," they use client-streaming, not duplex.

3. **Codec asymmetry** — `Codec` enum is JSON-only (`mesh_rpc.rs:79`). Anything richer (postcard, prost) goes through the raw bytes path. Should this plan also add a `Codec::Postcard` variant so the typed wrappers can do binary codecs without dropping to raw? Probably yes, but scope it as a follow-up — the typed wrappers in this plan compose against whatever `Codec` exists.

4. **Per-chunk headers** — REQUEST_CHUNK carries an optional headers vec. Useful for trace span continuity across long uploads, less useful day-to-day. Cap at 0 by default (`headers_count = 0`), allow up to `MAX_RPC_HEADERS` for callers that want them. Decide whether the typed `RequestStreamTyped<Req>` exposes per-chunk headers (extra type complexity) or hides them (simpler, but then user needs the raw path for header inspection). Lean: hide them in the typed surface; expose via the raw path.

5. **Cancellation observability** — does the server's request stream surface a distinct `RpcError::Cancelled` versus the caller dropping cleanly via `REQUEST_END`? Today the response-side `RpcStream::Drop` emits CANCEL. For duplex, the caller may legitimately want to half-close (no more sends, but keep receiving responses); `finish_sending()` handles that. Need to make sure `finish_sending` emits `REQUEST_END`, NOT `CANCEL` — those are semantically different. Pin in the duplex test suite.

6. **Replica routing under client-streaming** — `RoutingPolicy` (sticky / round-robin / etc.) is per-call; the routing decision happens once on the initial REQUEST. All subsequent REQUEST_CHUNKs go to the same target. This is correct, but worth documenting: "for client-streaming, the routing policy is consulted once at call open; chunks ride to that target only." Sticky routing's promise still holds.

## Acceptance criteria

**Substrate layer** (Phases A-D):

- New wire constants (`DISPATCH_RPC_REQUEST_CHUNK`, `DISPATCH_RPC_REQUEST_GRANT`, `FLAG_RPC_CLIENT_STREAMING_REQUEST`, `FLAG_RPC_REQUEST_END`) round-trip cleanly through `cortex::rpc` codec with full malformed-payload rejection coverage.
- `RpcStreamingRequestFold` accepts REQUEST + N × REQUEST_CHUNK + final FLAG_END, delivers them to the handler in order, observes CANCEL, surfaces handler panics as `Internal`.
- Request-direction window-grant flow control symmetric to the existing response-side: server's pump task stalls on `Semaphore::acquire_owned().await` when credits exhausted; `DISPATCH_RPC_REQUEST_GRANT` adds permits.
- Back-compat: pre-fix peers receive new dispatch IDs as `UnknownVersion` (logged, fold survives), and existing server-streaming tests continue to pass without modification.

**Veneer layer** (Phase E):

- `Mesh::call_client_stream_typed` round-trips at least 1000 chunks under real network handshake, with the typed handler observing all 1000 in order via the flattened `RequestStreamTyped`.
- `Mesh::call_duplex_typed` interleaves bidirectional traffic without deadlock under back-pressure on both directions.
- `DuplexCall::into_split` lets a duplex call run with the sink and stream in separate tokio tasks; CANCEL fires only when both halves drop.
- `ChunkedRequestStream::next()` surfaces `Chunk::Init` / `Chunk::Data` / `Chunk::End` correctly classified from the substrate's flag-tagged frames, pinning the SDK-internal `Chunk<T>` semantics.
- Zero substrate-level types leak into user-facing examples (`nrpc_client_stream.rs` and `nrpc_duplex.rs` mention only `Mesh`, `Codec`, `RequestStreamTyped`, `ClientStreamCall`, `DuplexCall`; no `Bytes`, no flag constants, no `Chunk<T>`).

**Cross-cutting:**

- `nrpc_client_streaming` and `nrpc_duplex` benches join the bench suite from this branch.
- Existing 4 SDK streaming tests (server-streaming) pass unmodified — back-compat verified.
