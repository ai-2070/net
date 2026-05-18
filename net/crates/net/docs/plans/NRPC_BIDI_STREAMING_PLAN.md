# nRPC bidirectional streaming plan — client streaming, server-streaming for client-streamed requests, and full duplex

Spec for the three missing nRPC streaming shapes. Reference:
- `net/crates/net/docs/misc/NRPC_DESIGN.md` (architectural framing + Phase 3 server-streaming details)
- `net/crates/net/sdk/src/mesh_rpc.rs` (the SDK surface that ships server-streaming today)
- `net/crates/net/src/adapter/net/cortex/rpc.rs` (codec, wire format, server/client folds)

## Status

- ✅ **Unary** `Mesh::call_typed` / `serve_rpc_typed` — one Req, one Resp.
- ✅ **Server-streaming** `Mesh::call_streaming_typed` / `serve_rpc_streaming_typed` — one Req, many Resp. Window-grant flow control wired (`DISPATCH_RPC_STREAM_GRANT`, header `nrpc-stream-window-initial`).
- ❌ **Client-streaming** — many Req, one Resp. No SDK surface, no wire support.
- ❌ **Duplex** — many Req ↔ many Resp interleaved. No SDK surface, no wire support.
- ❌ **Server-side handler for client-streamed requests** — `RpcHandler` today only receives a single decoded `RpcRequestPayload`. No stream-of-requests primitive on the fold.

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

Same pattern as existing typed surfaces:

```rust
impl Mesh {
    pub fn serve_rpc_client_stream_typed<Req, Resp, F, Fut>(
        &self, service: &str, codec: Codec, handler: F,
    ) -> Result<ServeHandle, ServeError>
    where
        Req: DeserializeOwned + Send + Sync + 'static,
        Resp: Serialize + Send + Sync + 'static,
        F: Fn(RequestStreamTyped<Req>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Resp, String>> + Send + 'static;

    pub fn serve_rpc_duplex_typed<Req, Resp, F, Fut>(
        &self, service: &str, codec: Codec, handler: F,
    ) -> Result<ServeHandle, ServeError>
    where
        F: Fn(RequestStreamTyped<Req>, ResponseSinkTyped<Resp>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), String>> + Send + 'static;

    pub async fn call_client_stream_typed<Req, Resp>(
        &self, target_node_id: u64, service: &str, opts: CallOptionsTyped,
    ) -> Result<ClientStreamCall<Req, Resp>, RpcError>;

    pub async fn call_duplex_typed<Req, Resp>(
        &self, target_node_id: u64, service: &str, opts: CallOptionsTyped,
    ) -> Result<DuplexCall<Req, Resp>, RpcError>;
}
```

`RequestStreamTyped<Req>` decodes each chunk via the captured `Codec`; decode failure terminates the stream with one `Err(RpcError::Codec)`, mirroring `RpcStreamTyped<Resp>`. The encoded-bytes path (raw `serve_rpc_client_stream` / `call_client_stream`) is the substrate the typed surface composes on, exposed for users who manage their own serialization (proto / postcard / hand-rolled).

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

Each phase is independently mergeable, with tests, and leaves the existing surfaces unchanged.

### Phase A — wire format

- Add `DISPATCH_RPC_REQUEST_CHUNK` (0x15), `DISPATCH_RPC_REQUEST_GRANT` (0x16) constants.
- Add `FLAG_RPC_CLIENT_STREAMING_REQUEST` (0x0010), `FLAG_RPC_REQUEST_END` (0x0020).
- Add `nrpc-request-window-initial` header.
- Codec + decoder for `RpcRequestChunkPayload` and `RpcRequestGrantPayload`. `encode_request_chunk` / `decode_request_chunk` / `encode_request_grant` / `decode_request_grant` helpers.
- Test fixtures: 5 wire-stability tests on the new payloads (round-trip, truncation rejection, `body` cap, `headers` cap, malformed-flag rejection).

### Phase B — server fold for client-streaming

- `RpcStreamingRequestFold` (parallel to `RpcServerStreamingFold`).
- `RpcClientStreamingHandler` trait + `RequestStream` type.
- `MeshNode::serve_rpc_client_stream(service, handler) -> ServeHandle`. Auto-registers `<service>.requests` exact-match + `<service>.replies.` prefix (same as today's `serve_rpc`).
- Cancellation: REQUEST_END closes the stream; CANCEL flips the token and surfaces one `Err(RpcError::Cancelled)` on the stream.
- Tests: 5 integration tests (`tests/integration_nrpc_client_streaming.rs`): basic round-trip with N=10 chunks, REQUEST_END semantics, CANCEL surfaces to handler, handler `Err` round-trips, handler panic surfaces as Internal.

### Phase C — caller side for client-streaming

- `ClientStreamCall<Req, Resp>` opaque type with `send` / `finish` / `grant_request_window`.
- `MeshNode::call_client_stream(target, service, opts) -> Result<ClientStreamCallRaw, RpcError>` raw-bytes substrate.
- `PendingEntry::ClientStreaming` variant on `RpcClientPending`.
- Caller-side request-window grant: helper to emit `REQUEST_GRANT` events from the server fold when the handler's stream consumer is slower than the caller's send rate.
- Tests: 4 integration tests through real network: caller sends 10 → server collects 10 → caller sees terminal Resp; caller drops mid-send → server sees CANCEL; caller closes via `finish` with 0 sends → server's stream yields one None, handler returns terminal Resp; window-grant throttles caller send when server is slow.

### Phase D — duplex (composes B + C)

- `RpcDuplexHandler` trait. `RpcStreamingRequestFold` extended to thread the response sink through to the handler (one new variant, no new fold type).
- `DuplexCall<Req, Resp>` caller-side surface with `into_split`.
- `PendingEntry::Duplex` variant.
- Tests: 6 integration tests: bidirectional echo (caller sends 5 / server echoes each / caller sees 5 then EOF); interleaved send-while-receiving; server finishes first (caller's send after server EOF errors cleanly); caller finishes first (server can still emit responses); `into_split` lets sink + stream live in separate tasks; CANCEL from either side closes both halves.

### Phase E — SDK typed wrappers

- `serve_rpc_client_stream_typed` / `serve_rpc_duplex_typed` / `call_client_stream_typed` / `call_duplex_typed` on `Mesh`.
- `RequestStreamTyped<Req>` (decode-on-poll, decode-failure-terminates).
- Re-exports + `mesh_rpc.rs` doc comments.
- Tests: 4 SDK tests pinning the typed-handler trait, application-error mapping, codec round-trip, and `into_split` lifecycle.

### Phase F — benchmarks (extends the suite landed in this branch)

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

Plan is done when:

- `Mesh::call_client_stream_typed` round-trips at least 1000 chunks under real network handshake, with the typed handler observing all 1000 in order.
- `Mesh::call_duplex_typed` interleaves bidirectional traffic without deadlock under back-pressure on both directions.
- `into_split` lets a duplex call run with the sink and stream in separate tokio tasks.
- CANCEL from either side closes both halves cleanly; pinned by a regression test.
- `nrpc_client_streaming` and `nrpc_duplex` benches run as part of the suite.
- Existing 4 SDK streaming tests (server-streaming) still pass without modification — back-compat verified.
