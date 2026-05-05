# nRPC — request/response as a Net subprotocol

Plan for a first-class request/response primitive on top of the existing mesh transport. Closes the structural gap that today forces every microservice consumer to roll its own correlation IDs, deadlines, retries, and cancellation on top of pub/sub channels.

**Architectural anchor: nRPC is a subprotocol, not a new packet shape.** The mesh already has a `subprotocol_id: u16` slot in `NetHeader`, a manifest-negotiation handshake (`SUBPROTOCOL_NEGOTIATION = 0x0600`), and an automatic capability tag (`subprotocol:0x0NNN`) so peers can discover support via the existing `CapabilityIndex`. nRPC slots in alongside `causal`, `migration`, `snapshot`, and `stream-window`. **No wire-format bump is required**; nRPC ships as one new subprotocol ID with its own framed payload.

## What's already there

- **Subprotocol machinery** — `SubprotocolDescriptor` (id + name + version + min_compatible), `SubprotocolRegistry`, manifest exchange via `negotiate()`, automatic capability-tag plumbing. nRPC adopts this verbatim; we add `pub const SUBPROTOCOL_NRPC: u16 = 0x0C00;` and a descriptor.
- **Reliable encrypted streams** — `MeshNode::open_stream_with(peer, reliable=true, ...)` plus `StreamWindow` (the `SUBPROTOCOL_STREAM_WINDOW` subprotocol) for receiver-authoritative credit grants. Streams are multiplexed over the existing AEAD session, so opening many of them is cheap (no new UDP socket, no new crypto context).
- **Identity & authz** — `EntityKeypair`, `OriginStamp`, signed capability announcements, `PermissionToken` with `TokenScope`. Server-side `RpcContext::caller` will be the AEAD-verified `EntityId`, not a self-claimed value in the request payload.
- **Per-daemon dispatch** — `DaemonRuntime::deliver(origin_hash, event)` is the existing typed unicast path inside one process. nRPC's server-side handler dispatch reuses this shape (keyed by service name instead of `origin_hash`).
- **Capability announcements** — periodically broadcast, signed, hop-counted. Today they advertise channels and modalities. nRPC extends them with a `services: Vec<ServiceDescriptor>` field for Layer 7 (Phase 2 of this plan).
- **Health & latency signals** — heartbeat loop, `RecoveryManager` retry-counted failure state, `proximity.rs` exposes per-route p50. Phase 2's routing policies consume these directly.

## Architecture

Two new pieces, both layered on top of what exists:

```text
Layer 7 (capabilities):
  CapabilityAnnouncement gains a `services: Vec<ServiceDescriptor>`
  field. Receivers index `(entity_id, service_name) → ServiceDescriptor`
  in a local ServiceRegistry. Same TTL as the rest of the auth
  surface; no new wire kind.

Layer 6 (subprotocols):
  SUBPROTOCOL_NRPC = 0x0C00. Stream-based: every call opens one
  reliable stream tagged with this id; the stream's frames are the
  request, response, and (Phase 3) intermediate streaming responses.
  Stream-close = call done; stream-reset = cancellation.

Layer ≤ 5 (transport, sessions, AEAD, channels):
  Unchanged.
```

Stream lifecycle == call lifecycle is the load-bearing simplification. We get correlation (stream id), cancellation (stream reset), backpressure (`StreamWindow`), ordered delivery, and per-call resource accounting from the reliable-stream layer for free. The nRPC subprotocol only has to define the *frame format* inside the stream.

## Wire format (in-stream framing)

Each call opens one reliable stream tagged with `subprotocol_id = SUBPROTOCOL_NRPC`. Frames inside the stream are length-delimited (`u32 LE` length prefix + frame body). All frames share a 1-byte type tag.

### `RequestFrame` (0x01) — first frame on the stream, caller → server

```text
┌──────┬─────────┬──────────────┬────────┬────────────┬──────────┬─────────┐
│ tag  │ version │ deadline_ns  │ flags  │ service    │ headers  │ payload │
│ 0x01 │  1 B    │ 8 B (u64 LE) │  2 B   │ varint+B   │ varint+B │  bytes  │
└──────┴─────────┴──────────────┴────────┴────────────┴──────────┴─────────┘
```

- **`version`** — `1`. Future bumps additive; readers reject unknown via `ResponseFrame{status: UnknownVersion}` and close the stream. (Per-subprotocol versioning is also exchanged at manifest-negotiation time, so version mismatches are usually caught earlier.)
- **`deadline_ns`** — absolute deadline (unix nanos). `0` means "no deadline; cancel via stream reset only." Server short-circuits with `Timeout` if `now_ns() > deadline_ns` before starting work.
- **`flags`** — `u16` bitfield:
  - `bit 0` (`IDEMPOTENT`) — request is safe for the server to dedup against an idempotency key carried in headers.
  - `bit 1` (`STREAMING_RESPONSE`) — server may emit multiple `ResponseFrame`s on this stream (Phase 3). Without it, the first `ResponseFrame` ends the call.
  - `bit 2` (`PROPAGATE_TRACE`) — request carries `traceparent` / `tracestate` headers.
  - `bits 3–15` reserved, must be zero on write, ignored on read.
- **`service`** — varint length + UTF-8 bytes (max 256). Server-side dispatch key.
- **`headers`** — varint count + `(name, value)` pairs (each varint-length-prefixed). Used for trace context, idempotency key, content-type hints.
- **`payload`** — opaque bytes. Caller serializes (JSON, postcard, protobuf — caller's choice).

No `request_id` field — the stream id is the correlation id. Cancellation = `stream.reset()`. Deadline expiry = caller resets the stream after the local timer fires; server's handler observes the reset via its `CancellationToken`.

### `ResponseFrame` (0x02) — server → caller

```text
┌──────┬─────────┬────────┬──────────┬─────────┐
│ tag  │ version │ status │ headers  │ payload │
│ 0x02 │  1 B    │  2 B   │ varint+B │  bytes  │
└──────┴─────────┴────────┴──────────┴─────────┘
```

- **`status`** — `u16`:
  - `0x0000` — `Ok` (terminal unless `STREAMING_RESPONSE`)
  - `0x0001` — `NotFound` (no service registered with that name)
  - `0x0002` — `Unauthorized` (token doesn't include the requested service)
  - `0x0003` — `Timeout` (server observed deadline expired before starting)
  - `0x0004` — `Backpressure` (per-service queue full)
  - `0x0005` — `Cancelled` (caller closed the stream before server completed)
  - `0x0006` — `Internal` (handler panicked or returned an error)
  - `0x0007` — `UnknownVersion` (request version not supported by server)
  - `0x0008..0x7FFF` — reserved
  - `0x8000..0xFFFF` — application-defined (server's choice)
- **`headers`** — varint count + `(name, value)` pairs. v1 has a small enumerated set; unknown headers are passed through.
- **`payload`** — opaque bytes; meaning depends on `status`. For `Ok` it's the application response; for the error variants it's a UTF-8 diagnostic.

For unary calls: server emits one `ResponseFrame`, then closes the stream gracefully. For Phase 3 streaming responses: server emits N `ResponseFrame`s with `status = Ok`, then closes the stream (or emits a terminal `ResponseFrame` with a non-`Ok` status to signal an in-flight error).

### Why no `RpcCancel` packet kind

The first design proposed an `RpcCancel` packet at the mesh-frame layer. With stream-based framing it's unnecessary: dropping the caller's `RpcCall` future calls `stream.reset()`, which the server observes as a stream-level event without a new packet kind. Same for deadline expiry — caller-side timer fires, caller resets the stream.

Total fixed wire overhead per call: ~14 B for the request prefix (version + deadline + flags + service-length + headers-length + payload-length) and ~6 B for the response prefix, before the payload. Comparable to gRPC framing.

## API surface

### Caller side (`Mesh::call`)

```rust
impl Mesh {
    /// Direct entity-to-entity unary call. Opens a reliable stream
    /// tagged SUBPROTOCOL_NRPC, sends the request frame, awaits the
    /// response frame, closes the stream.
    pub async fn call(
        &self,
        target: &EntityId,
        service: &str,
        payload: Bytes,
        opts: CallOptions,
    ) -> Result<RpcReply, RpcError>;

    /// Service-name dispatch (Phase 2). Looks up a healthy instance
    /// from the local ServiceRegistry, then delegates to `call`.
    pub async fn call_service(
        &self,
        service: &str,
        payload: Bytes,
        opts: CallOptions,
    ) -> Result<RpcReply, RpcError>;
}

#[derive(Debug, Clone, Default)]
pub struct CallOptions {
    pub deadline: Option<Instant>,
    pub idempotency_key: Option<u64>,
    pub trace_context: Option<TraceContext>,
    /// Caller-side semaphore on concurrent in-flight calls per
    /// (local, target) pair. Default 64; bounds local resource
    /// exhaustion when a downstream stalls.
    pub max_in_flight_per_target: u32,
}

#[derive(Debug, Clone)]
pub struct RpcReply {
    pub payload: Bytes,
    pub headers: HashMap<HeaderName, HeaderValue>,
    pub latency_ns: u64,
    pub server_entity: EntityId,
}

#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    #[error("no route to target")]
    NoRoute,
    #[error("timeout after {elapsed_ms}ms")]
    Timeout { elapsed_ms: u64 },
    #[error("server returned {status:?}: {message}")]
    ServerError { status: RpcStatus, message: String },
    #[error("local cancellation")]
    Cancelled,
    #[error("subprotocol negotiation failed: peer does not speak nRPC")]
    SubprotocolUnsupported,
    #[error("transport: {0}")]
    Transport(#[from] TransportError),
}
```

The future returned by `call()` is the call. Drop = `stream.reset()` = cancellation. `await` = wait for the first `ResponseFrame` then close. Deadline behavior: if `opts.deadline` is set, the future races a `tokio::time::sleep_until`; whichever fires first wins, and a timer-driven loss resets the stream.

The `SubprotocolUnsupported` error is the new failure mode introduced by riding on subprotocol negotiation — a peer whose manifest exchange didn't list `0x0C00` cannot accept nRPC calls. This is detected at the mesh layer at session-establishment time, so callers see it as a fast-fail rather than a timeout.

### Server side (`Mesh::serve`)

```rust
impl Mesh {
    /// Register a handler for `service`. Multiple registrations for
    /// the same service on one node are an error (use replica/fork
    /// groups for that). Returns a `ServeHandle` whose Drop deregisters.
    pub fn serve<F, Fut>(&self, service: &str, handler: F) -> Result<ServeHandle, ServeError>
    where
        F: Fn(RpcContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Bytes, RpcHandlerError>> + Send;
}

pub struct RpcContext {
    pub caller: EntityId,            // AEAD-verified
    pub service: String,
    pub payload: Bytes,
    pub headers: HashMap<HeaderName, HeaderValue>,
    pub deadline: Option<Instant>,
    pub cancellation: CancellationToken,
    pub trace_context: Option<TraceContext>,
}

#[derive(Debug, thiserror::Error)]
pub enum RpcHandlerError {
    #[error("application error {code:#06x}: {message}")]
    Application { code: u16, message: String },
    #[error("handler panicked")]
    Panic,
    #[error("internal: {0}")]
    Internal(String),
}
```

Server-side dispatch is straightforward: the nRPC subprotocol handler intercepts new streams tagged `SUBPROTOCOL_NRPC`, reads the first frame (must be `RequestFrame`), looks up `service` in the per-node `serve` registry, spawns the handler with an `RpcContext`, and writes the returned `Bytes` back as a `ResponseFrame`. Stream reset from the caller flips the `cancellation` token.

### `nRPC` over the SDK

```rust
impl DaemonRuntime {
    pub fn rpc(&self) -> RpcClient;
}

pub struct RpcClient { /* shares the underlying Mesh */ }

impl RpcClient {
    pub async fn call<Req, Resp>(
        &self,
        target: &EntityId,
        service: &str,
        request: &Req,
    ) -> Result<Resp, RpcError>
    where
        Req: Serialize,
        Resp: DeserializeOwned;
}
```

Higher-level than the raw `Bytes` API, with codec selection (`serde_json`, `postcard`) via an `RpcCodec` enum on the client. Bindings (Node / Python / Go) get parallel typed surfaces; in Node the codec is JSON by default.

## Service discovery (Layer 7, Phase 2)

### Capability-announcement extension

`CapabilityAnnouncement` gains a `services` field. Each receiving node accumulates `(entity_id, service_name) → ServiceDescriptor` into a local `ServiceRegistry`:

```rust
pub struct ServiceDescriptor {
    pub name: String,                  // unique within node
    pub version: SemVer,               // for compatible-version routing
    pub schema_hash: Option<[u8; 32]>, // advisory; readers may dedup
    pub max_in_flight: u32,            // server-side queue cap
    pub flags: ServiceFlags,           // IDEMPOTENT, STATELESS, etc.
}
```

Announcements ride the existing signed-capability infrastructure. Discovery requires no new wire kind.

The `subprotocol:0x0C00` capability tag (auto-generated by `SubprotocolDescriptor::capability_tag()`) signals "this node speaks nRPC" — a separate signal from "this node serves these services." Both are useful: the subprotocol tag lets routers prefer nRPC-capable hops; the service descriptors let `Mesh::call_service` find a target.

### Routing policy

`Mesh::call_service(name, ...)` consults the local registry:

1. Filter to instances whose `version` matches the caller's required range.
2. Drop instances whose `proximity::node_health(entity_id)` shows `Unhealthy`.
3. Apply the configured `RoutingPolicy`:
   - `RoundRobin` — even distribution.
   - `PowerOfTwoChoices` — pick two at random, send to the one with lower in-flight count. Default.
   - `Sticky { key }` — hash the key, pick the consistent-hashed instance. Session affinity.
   - `LowestLatency` — pick the instance with the lowest p50 from `proximity.rs`.
4. Apply caller-side per-target concurrency cap (semaphore from `CallOptions`).

Health is passive (heartbeat-driven) plus active (RPC error-rate threshold per target); unhealthy instances cool off for `health_recovery_ms` before re-entering the routing pool.

### Service-level authorization

Extend `TokenScope` with a `RPC_CALL` bit and add a `services: Vec<String>` allowlist on `PermissionToken`. Servers reject calls whose token doesn't list the requested service in scope; the rejection is `RpcStatus::Unauthorized`. Empty allowlist means "no services allowed" (defense-in-depth default; existing tokens without the field don't authorize RPC).

## Backpressure, concurrency, cancellation

- **Per-stream backpressure (free).** Each call's stream is governed by `StreamWindow` — receiver-authoritative credit grants under `SUBPROTOCOL_STREAM_WINDOW`. Large request or response payloads ride the existing window mechanism without additional flow control in nRPC.
- **Server-side per-service queue.** Each `serve()` registration gets a `tokio::sync::Semaphore` sized to `ServiceDescriptor::max_in_flight`. `acquire_owned` happens on the dispatcher before the handler is spawned; over-cap requests get an immediate `ResponseFrame{status: Backpressure}` and the stream closes.
- **Server-side per-caller concurrency cap.** Separate from the per-service cap, a per-(caller, service) counter prevents one slow caller from starving others. Defaults to `min(8, max_in_flight / 4)`.
- **Caller-side per-target semaphore.** `CallOptions::max_in_flight_per_target` (default 64) bounds the in-flight call count per `(local, target)` pair, preventing local resource exhaustion when a downstream stalls.
- **Cancellation propagation.** Caller drops the `RpcCall` future → stream reset → server's handler sees `cancellation.cancelled().await` fire. Both sides clean up their state-machine entries on cancellation, on `ResponseFrame` arrival, or on the per-call deadline. No `RpcCancel` packet kind needed.

## Tracing

W3C Trace Context style. The request's `traceparent` and `tracestate` headers (in the `RequestFrame::headers` block) propagate through the call:

- Server appends to its own span tree using whatever exporter the operator has configured; nRPC defines no exporter itself (interop with `tracing-opentelemetry`, Datadog, etc. lives in user code).
- Hedged or retried requests propagate the same `trace_id` with a fresh `span_id` per attempt.
- Discovery lookups, queue waits, and handler latency are instrumented as nested spans by the runtime; operators get end-to-end traces without instrumenting their handlers.

## Phasing

| Phase | Release | Scope |
|------|---------|-------|
| **1** | v0.12 | `SUBPROTOCOL_NRPC = 0x0C00` registered. Frame codec (`RequestFrame`, `ResponseFrame`). Direct entity-to-entity unary `Mesh::call(target, ...)` + `Mesh::serve(name, ...)`. Per-call deadline, cancellation via stream-reset, in-flight semaphores. Token-scope check (`RPC_CALL`). Test suite covering correlation (stream id), timeout, cancel, backpressure-rejection, server panic, subprotocol-unsupported peer. |
| **2** | v0.13 | Service registry + capability-announcement extension. `Mesh::call_service(name, ...)` with routing policies (RoundRobin, P2C, Sticky, LowestLatency). Health-aware filtering. Per-(caller, service) concurrency cap. SDK typed wrappers (Rust + Node + Python + Go bindings). |
| **3** | v0.14 | Server-streaming responses (`STREAMING_RESPONSE` flag, multiple `ResponseFrame`s per stream). Caller-side helpers: `with_retry(policy)`, `with_circuit_breaker(...)`, `with_hedge(n)`. W3C Trace Context propagation hardened (interop tests against OpenTelemetry collector). Per-call latency / error-rate metrics on a Prometheus-compatible endpoint. |
| **deferred** | v0.15+ | Client-streaming, bidirectional streaming, schema registry / IDL codegen (`.nrpc` files → typed Rust/TS/Python clients). |

Each phase ships independently. Phase 1 is materially smaller than the original (no new packet kinds, no wire bump) — the bulk is the frame codec, the stream-handler hookup, and the per-call state machine.

## Authorization model

Two layers, both load-bearing:

1. **Subprotocol-level admission.** A peer that didn't advertise `SUBPROTOCOL_NRPC` in its manifest is filtered at session-establishment time; calls return `SubprotocolUnsupported` immediately. This is automatic from the existing manifest-negotiation flow.
2. **Service-level allowlist.** `PermissionToken::rpc_services: Vec<String>` lists the services this token may call. Empty list means none. Server rejects with `Unauthorized` on mismatch. `*` is allowed as "any service" for trusted introspection / admin tokens.

End-to-end identity: every `RpcContext::caller` is the verified `EntityId` from the AEAD session, not a self-claimed value in the request payload. nRPC inherits the mesh's existing in-channel-identity-spoofability tradeoff (see `adapter/net/identity/origin.rs` doc comment) — within an authenticated channel, any peer can mint a request claiming an arbitrary `origin_hash`. Callers needing end-to-end origin authentication must layer a signed envelope inside the payload.

## Test surface

### Phase 1
- **Stream-id correlation.** Spawn N concurrent `call()` futures against one target; assert each gets its own response on its own stream. Pin that out-of-order responses arrive at the right callers.
- **Deadline expiration.** Server that sleeps past the deadline produces `Timeout` on the caller side, AND the server's handler sees `cancellation.cancelled()` fire when the deadline passes.
- **Caller cancellation.** Dropping the `RpcCall` future before the response arrives closes the stream; server's handler observes the token and aborts.
- **Server panic.** Handler that panics surfaces as `RpcStatus::Internal` to the caller (runs inside `catch_unwind`); the stream still closes cleanly.
- **Backpressure rejection.** Fill the per-service queue past `max_in_flight`; over-cap caller gets `ResponseFrame{status: Backpressure}` immediately, no queueing, stream closes.
- **Token scope rejection.** Token without `RPC_CALL` gets `Unauthorized`; with `RPC_CALL` but missing the service in the `rpc_services` allowlist, also `Unauthorized`.
- **Subprotocol negotiation.** A peer whose manifest didn't include `0x0C00` causes `call()` to return `SubprotocolUnsupported` at the mesh layer, before any stream is opened.
- **Frame version negotiation.** A v2 server receiving a v1 request handles it; a v1 server receiving a v2 request returns `ResponseFrame{status: UnknownVersion}` and closes. Subprotocol-version negotiation at manifest time should normally prevent this, but the in-frame guard is the floor.
- **Frame round-trip.** `RequestFrame::to_bytes` + `from_bytes` for every field combination; reject malformed inputs (truncated, bad tag, garbage flags).
- **Identity guard.** `RpcContext::caller` is the AEAD-verified peer entity, not the value in the payload.

### Phase 2
- **Service-descriptor propagation.** Service descriptors travel through capability announcements; receivers register them with the right TTL; expired entries are pruned.
- **Routing policy correctness.** `PowerOfTwoChoices` distributes load across N healthy instances; `Sticky` is consistent across calls with the same key; `LowestLatency` picks the lowest-p50 instance.
- **Health-aware filtering.** Instances marked unhealthy are excluded; recovery puts them back after `health_recovery_ms`.
- **Per-(caller, service) concurrency.** One slow caller can't starve others; cap fires per-caller, not globally.
- **Migration interaction.** When a daemon migrates (compute layer), its serving capacity follows; in-flight RPCs get `Cancelled` rather than disappearing silently.

### Phase 3
- **Server-streaming order.** N `ResponseFrame`s arrive in order; cancellation mid-stream cleanly closes both ends with no orphaned in-flight handler.
- **Trace context propagation.** `traceparent` / `tracestate` headers round-trip; hedged retries share `trace_id` but emit fresh `span_id`s.
- **Hedged requests.** Hedge of N=2 wins on the faster reply, cancels the loser via stream reset.
- **Circuit breaker.** `OPEN` state short-circuits calls; `HALF_OPEN` probes; `CLOSED` resumes; transitions match the documented state machine.

## Out of scope

- **Pub/sub replacement.** nRPC complements channels, doesn't replace them. Event-bus use cases stay on channels.
- **Service-mesh sidecar.** nRPC runs in-process. There's no plan for a separate sidecar binary that intercepts traffic à la Istio / Linkerd.
- **Mutual TLS / cert rotation.** Net's existing AEAD + capability-token model is the auth substrate; nRPC inherits it.
- **Schema-validated payloads in v1.** Payloads are `Bytes`. Optional schema registry is a deferred follow-up; until then, application code owns serde.
- **Sync RPC.** Every API is async-only; no blocking `call_blocking` shape.

## Open design questions

These are the calls I want sanity-checked before code lands:

1. **Subprotocol ID assignment.** The existing IDs cluster around `0x0400` (causal), `0x0500-ish` (snapshot), `0x0600` (negotiation), `0x0B00` (stream-window). `0x0C00` is the proposed slot for nRPC. If there's a numbering convention I haven't pieced together (e.g., reserve `0x0C00..0x0CFF` for service-mesh primitives), say so.

2. **Stream-per-call vs. multiplexed control stream.** Stream-per-call (this plan) is simple and gives natural cancellation, but at extreme RPS each call still has stream-state-machine setup cost. An alternative — one persistent control stream per peer carrying all in-flight calls demuxed by an in-frame `call_id` — has lower per-call cost but reintroduces the correlation/cancellation/backpressure machinery the original design proposed. Recommend stream-per-call; revisit if a real workload trips on the per-stream cost.

3. **`RpcStatus` numbering.** Should `0x0001..0x7FFF` mirror gRPC status codes (`NotFound = 5`, etc.) for operator familiarity, or be a Net-native enumeration? Mirroring eases interop with existing tooling but locks us into gRPC's shape forever. Recommend Net-native with documented gRPC-equivalence in this doc, not at the wire level.

4. **`CallOptions::deadline` as `Instant` vs `Duration`.** `Instant` is unambiguous about clock semantics but doesn't encode well across the wire (the server's `Instant` is incomparable). The wire carries `unix_nanos`; the API takes `Instant` and converts at the call boundary. Document that "deadline" is a hint to the server, not a contract — server may complete after the deadline if it didn't observe cancellation in time.

5. **Hedging in v1?** Implementing it requires the cancellation machinery to be airtight (the loser's response must be discarded cleanly, the loser's stream cleanly reset). Phase 3 is the right home, but if we want hedging earlier, Phase 1's cancellation tests are the prerequisite. Plan above keeps hedging in Phase 3.

6. **Idempotency-key location.** Original plan put `idempotency_key: u64` in the request prefix; this revision moves it into headers (varint name + value). Headers compose better with future additions (TTL, dedup-window-override) but cost a few more bytes. Keep in headers.
