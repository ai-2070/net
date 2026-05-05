# nRPC — request/response on the Net mesh

Plan for a first-class request/response primitive on top of the existing mesh transport. Closes the structural gap that today forces every microservice consumer to roll its own correlation IDs, deadlines, retries, and cancellation on top of pub/sub channels. nRPC sits between the reliable-stream layer and the daemon-dispatch layer; it does not replace either.

## What's already there

- **Reliable encrypted streams** — `MeshNode::open_stream_with(peer, reliable=true, ...)` plus `StreamWindow` for in-stream backpressure (`tx_credit_remaining`, `RxCreditState`).
- **Identity & authz** — `EntityKeypair`, `OriginStamp`, signed capability announcements, `PermissionToken` with `TokenScope`.
- **Per-daemon dispatch** — `DaemonRuntime::deliver(origin_hash, event)` is the existing typed unicast path. Inside one process the runtime already knows how to route a payload to a specific daemon by `origin_hash`.
- **Capability announcements** — periodically broadcast, signed, hop-counted. Today they advertise channels + modalities.
- **Health signals** — heartbeat loop, `RecoveryManager` with retry-counted failure state, latency exposed via `proximity.rs`.

What's missing is the glue: a wire shape that pairs a request packet with its response, a correlation/timeout state machine, server-side dispatch by service name, and a discovery layer that turns "find a healthy instance of service X" into an `entity_id` plus address.

## Architecture

Three layers, each useful on its own:

```text
┌───────────────────────────────────────────────────────────┐
│  Layer 3: discovery                                       │
│    - service registry (name → [(entity_id, addr, health)])│
│    - lookup-with-policy (round-robin, p2c, sticky)        │
│    - capability-announcement extension for service descs  │
├───────────────────────────────────────────────────────────┤
│  Layer 2: nRPC                                            │
│    - RpcRequest / RpcResponse / RpcCancel packet kinds    │
│    - correlation-id state machine                         │
│    - per-call deadline + cancellation                     │
│    - server-side handler registry                         │
├───────────────────────────────────────────────────────────┤
│  Layer 1: existing mesh transport (unchanged)             │
│    - reliable streams, AEAD sessions, routing             │
│    - capability tokens, channel rosters                   │
└───────────────────────────────────────────────────────────┘
```

Layer 2 alone is enough for direct entity-to-entity RPC (caller knows the target's `entity_id`). Layer 3 layers service-name lookup on top. We ship Layer 2 first; Layer 3 in a follow-up.

## Wire format

Three new packet kinds at the mesh-frame layer. Each rides on its own AEAD-sealed packet (no new transport protocol):

### `RpcRequest` (v1, fixed prefix + variable payload)

```text
┌──────────┬─────────┬──────────────┬───────────────┬──────────┬─────────────────┬─────────┐
│  magic   │ version │ request_id   │ deadline_ns   │ flags    │ service_name    │ payload │
│  4 B     │  1 B    │  16 B (UUID) │  8 B (u64 LE) │  2 B     │  varint + bytes │  bytes  │
└──────────┴─────────┴──────────────┴───────────────┴──────────┴─────────────────┴─────────┘
```

- **`magic`** — `NRPC` (`0x4E 0x52 0x50 0x43`). Distinguishes from existing channel-publish packets at the dispatch layer.
- **`version`** — `1`. Future bumps additive; readers reject unknown versions.
- **`request_id`** — UUIDv7 (millisecond timestamp + random) so `request_id` is sortable for log correlation. Caller-generated, must be unique per (caller, target) for the lifetime of the call.
- **`deadline_ns`** — absolute deadline (unix nanos). `0` means "no deadline; caller will manage cancellation explicitly." Server short-circuits with `Timeout` if `now_ns() > deadline_ns` before starting work.
- **`flags`** — bitfield:
  - `bit 0` (`IDEMPOTENT`) — set if the request is safe to retry; server may dedup against `request_id` within its idempotency window.
  - `bit 1` (`STREAMING_RESPONSE`) — server may emit multiple `RpcResponse` packets for this request (Phase 3).
  - `bit 2` (`PROPAGATE_TRACE`) — request carries a `traceparent` header (W3C Trace Context); server appends to its own spans.
  - `bits 3–15` reserved, must be zero on write.
- **`service_name`** — varint length + UTF-8 bytes (max 256). Server-side dispatch key. Empty string means "addressed to the entity directly, no service routing" (Layer 2-only path).
- **`payload`** — opaque bytes. Caller serializes (JSON, postcard, protobuf — caller's choice). v1 does not enforce any schema.

### `RpcResponse`

```text
┌──────────┬─────────┬──────────────┬──────────┬───────────┬─────────┐
│  magic   │ version │ request_id   │ status   │ headers   │ payload │
│  4 B     │  1 B    │  16 B (UUID) │  2 B     │  varint+B │  bytes  │
└──────────┴─────────┴──────────────┴──────────┴───────────┴─────────┘
```

- **`status`** — `u16`:
  - `0x0000` — `Ok`
  - `0x0001` — `NotFound` (no service registered with that name on this node)
  - `0x0002` — `Unauthorized` (token doesn't include the requested service)
  - `0x0003` — `Timeout` (server observed deadline expired before starting)
  - `0x0004` — `Backpressure` (server's per-service queue full)
  - `0x0005` — `Cancelled` (caller sent `RpcCancel` before server completed)
  - `0x0006` — `Internal` (handler panicked or returned an error)
  - `0x0007` — `UnknownVersion` (request version not supported by server)
  - `0x0008..0x7FFF` — reserved for future use
  - `0x8000..0xFFFF` — application-defined error codes (server's choice)
- **`headers`** — varint count + `(name, value)` pairs. Used for trace context, content-type hints, cache directives. v1 has a small enumerated set; unknown headers are passed through unchanged.
- **`payload`** — opaque bytes; meaning depends on `status`. For `Ok` it's the application response; for the error variants it's a UTF-8 diagnostic string.

### `RpcCancel`

```text
┌──────────┬─────────┬──────────────┐
│  magic   │ version │ request_id   │
│  4 B     │  1 B    │  16 B (UUID) │
└──────────┴─────────┴──────────────┘
```

Sent by the caller when the `RpcCall` future is dropped or the deadline expires locally before the response arrives. Server's handler sees a tokio `CancellationToken` flip; if the handler hasn't started, it short-circuits without running.

Total fixed wire overhead: 31 B (request) / 23 B (response) / 21 B (cancel), before payload. Comparable to gRPC framing.

## API surface

### Caller side (`Mesh::call`)

```rust
impl Mesh {
    /// Direct entity-to-entity unary call (Layer 2).
    pub async fn call(
        &self,
        target: &EntityId,
        service: &str,
        payload: Bytes,
        opts: CallOptions,
    ) -> Result<RpcReply, RpcError>;

    /// Service-name dispatch (Layer 3, Phase 2). Looks up a healthy
    /// instance from the local registry, then delegates to `call`.
    pub async fn call_service(
        &self,
        service: &str,
        payload: Bytes,
        opts: CallOptions,
    ) -> Result<RpcReply, RpcError>;
}

#[derive(Debug, Clone)]
pub struct CallOptions {
    pub deadline: Option<Instant>,
    pub idempotency_key: Option<u64>,
    pub trace_context: Option<TraceContext>,
    pub max_in_flight_per_target: u32, // caller-side semaphore (default 64)
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
    #[error("transport: {0}")]
    Transport(#[from] TransportError),
}
```

`RpcCall` returned by `call()` is a future. Dropping it triggers `RpcCancel`; awaiting it yields the response or an error. Deadline behavior: if `opts.deadline` is set, the future races a `tokio::time::sleep_until`; whichever fires first wins.

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
    pub caller: EntityId,
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

The handler runs on the existing tokio runtime. The `cancellation` token is what makes server-side cancellation work — handlers must `.cancellation.cancelled().await` (or use `tokio::select!`) to abort cleanly. Handlers that ignore cancellation will run to completion and their response will be discarded.

### `nRPC` over the SDK

The Rust SDK adds:

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

Higher-level than the raw `Bytes` API, with `serde_json` / `postcard` codec selection via a `RpcCodec` enum on the client. Bindings (Node / Python / Go) get a parallel typed surface; in Node the codec is JSON by default with a JSON-Schema-validated variant available as opt-in.

## Service discovery (Layer 3, Phase 2)

### Capability-announcement extension

Today's `CapabilityAnnouncement` advertises channels and modalities. We add a `services: Vec<ServiceDescriptor>` field:

```rust
pub struct ServiceDescriptor {
    pub name: String,                  // unique within node
    pub version: SemVer,               // for compatible-version routing
    pub schema_hash: Option<[u8; 32]>, // advisory; readers may dedup
    pub max_in_flight: u32,            // server-side queue cap
    pub flags: ServiceFlags,           // IDEMPOTENT, STATELESS, etc.
}
```

Announcements ride the existing signed-capability infrastructure (no new wire kind for discovery itself). Each receiving node accumulates `(entity_id, service_name) → ServiceDescriptor` into a local `ServiceRegistry`. The registry expires entries when their parent capability announcement does — same TTL the rest of the auth surface uses.

### Routing policy

`Mesh::call_service(name, ...)` consults the local registry:

1. Filter to instances whose `version` matches the caller's required range.
2. Drop instances whose `proximity::node_health(entity_id)` shows `Unhealthy`.
3. Apply the configured `RoutingPolicy`:
   - `RoundRobin` — even distribution.
   - `PowerOfTwoChoices` — pick two at random, send to the one with lower in-flight count. Default.
   - `Sticky { key }` — hash the key, pick the consistent-hashed instance. For session affinity.
   - `LowestLatency` — pick the instance with the lowest p50 from `proximity.rs`.
4. Apply caller-side per-target concurrency cap (semaphore from `CallOptions`).

Health is passive (heartbeat-driven) plus active (RPC error-rate threshold per target); unhealthy instances cool off for `health_recovery_ms` before re-entering the routing pool.

### Service-level authorization

Extend `TokenScope` with a `RPC_CALL` bit and add a `services: Vec<String>` allowlist on `PermissionToken`. Servers reject calls whose token doesn't list the requested service in scope; the rejection is `RpcStatus::Unauthorized`. Empty allowlist means "no services allowed" (defense-in-depth default; existing tokens without the field don't authorize RPC).

## Backpressure, concurrency, cancellation

- **Server-side per-service queue** — each `serve()` registration gets a `tokio::sync::Semaphore` sized to `ServiceDescriptor::max_in_flight`. `acquire_owned` happens on the dispatcher thread before the handler is spawned; over-cap requests get an immediate `RpcStatus::Backpressure` reply.
- **Server-side per-caller concurrency cap** — separate from the per-service cap, a per-(caller, service) counter prevents one slow caller from starving others. Defaults to `min(8, max_in_flight / 4)`.
- **Caller-side per-target semaphore** — `CallOptions::max_in_flight_per_target` (default 64) bounds the in-flight call count per `(local, target)` pair, preventing local resource exhaustion.
- **Cancellation propagation** — caller's `RpcCall` drop sends `RpcCancel`; server's handler sees `cancellation.cancelled().await` fire. Both sides clean up their state-machine entries on cancellation, on `RpcResponse` arrival, or on the per-call deadline.
- **In-stream backpressure** — large request/response payloads (> 1 packet) ride on the existing reliable-stream backpressure (`StreamWindow`). The RPC layer doesn't add a second window.

## Tracing

W3C Trace Context style. The request's `traceparent` and `tracestate` headers (encoded in the `headers` block of the `RpcRequest`) propagate through the call:

- Server appends to its own span tree using whatever exporter the operator has configured; nRPC defines no exporter itself (interop with `tracing-opentelemetry`, Datadog, etc. lives in user code).
- Hedged or retried requests propagate the same `trace_id` with a fresh `span_id` per attempt.
- Discovery lookups, queue waits, and handler latency are instrumented as nested spans by the runtime; operators get end-to-end traces without instrumenting their handlers.

## Phasing

| Phase | Release | Scope |
|------|---------|-------|
| **1** | v0.12 | Wire shape (RpcRequest/Response/Cancel). Direct entity-to-entity unary `Mesh::call(target, ...)` + `Mesh::serve(name, ...)`. Per-call deadline, cancellation, in-flight semaphores. Token-scope check (`RPC_CALL`). Test suite covering correlation, timeout, cancel, backpressure-rejection, server panic, version mismatch. |
| **2** | v0.13 | Service registry + capability-announcement extension. `Mesh::call_service(name, ...)` with routing policies (RoundRobin, P2C, Sticky, LowestLatency). Health-aware filtering. Per-(caller, service) concurrency cap. SDK typed wrappers (Rust + Node + Python + Go bindings). |
| **3** | v0.14 | Server-streaming responses (`STREAMING_RESPONSE` flag, multiple `RpcResponse` packets per request). Caller-side helpers: `with_retry(policy)`, `with_circuit_breaker(...)`, `with_hedge(n)`. W3C Trace Context propagation hardened (interop tests against OpenTelemetry collector). Per-call latency / error-rate metrics on a Prometheus-compatible endpoint. |
| **deferred** | v0.15+ | Client-streaming, bidirectional streaming, schema registry / IDL codegen (`.nrpc` files → typed Rust/TS/Python clients). |

Each phase ships independently; consumers can adopt Phase 1 (direct call) without waiting for Phase 2 (discovery), and Phase 2 without Phase 3 (streaming).

## Authorization model

Two layers, both load-bearing:

1. **Channel-style admission (existing)** — the caller's token must include `RPC_CALL` scope to use the RPC machinery at all. This is the same gate that today protects publish/subscribe.
2. **Per-service allowlist (new)** — `PermissionToken::rpc_services: Vec<String>` lists the services this token may call. Empty list means none. Server rejects with `Unauthorized` on mismatch. `*` is allowed as "any service" for trusted introspection / admin tokens.

The server enforces both. The caller doesn't pre-check (the token is already cached locally; an enforcement bug in the caller's allowlist is harmless since the server is authoritative).

End-to-end identity: every `RpcContext::caller` is the verified `EntityId` from the AEAD session, not a self-claimed value in the request payload. nRPC inherits the mesh's existing in-channel-identity-spoofability tradeoff (see `adapter/net/identity/origin.rs` doc comment) — within an authenticated channel, any peer can mint a request claiming an arbitrary `origin_hash`. Callers needing end-to-end origin authentication must layer a signed envelope inside the payload.

## Test surface

Per phase, the regression coverage that gates the release:

### Phase 1
- **Correlation** — out-of-order responses arrive at the right callers. Spawn N concurrent `call()` futures against one target; assert each gets its own response.
- **Deadline expiration** — a server that sleeps past the deadline produces a `Timeout` on the caller side, and the server's handler sees `cancellation.cancelled()` fire when the deadline passes.
- **Caller cancellation** — dropping the `RpcCall` future before the response arrives sends `RpcCancel`; the server's handler observes the token and aborts.
- **Server panic** — handler that panics surfaces as `RpcStatus::Internal` to the caller (not as a UAF; runs inside `catch_unwind`).
- **Backpressure rejection** — fill the per-service queue past `max_in_flight`; the over-cap caller gets `RpcStatus::Backpressure` immediately, no queueing.
- **Token scope rejection** — a token without `RPC_CALL` gets `Unauthorized`; with `RPC_CALL` but missing the service in `rpc_services` allowlist, also `Unauthorized`.
- **Version negotiation** — a v2 server receiving a v1 request handles it; a v1 server receiving a v2 request returns `UnknownVersion`. Pin the floor.
- **Wire-format roundtrip** — `RpcRequest::to_bytes` + `from_bytes` for every field combination; reject malformed inputs (truncated, bad magic, garbage flags).
- **Identity guard** — `RpcContext::caller` is the AEAD-verified peer entity, not the value in the payload.

### Phase 2
- **Service discovery propagation** — service descriptors travel through capability announcements; receivers register them with the right TTL; expired entries are pruned.
- **Routing policy correctness** — `PowerOfTwoChoices` distributes load across N healthy instances; `Sticky` is consistent across calls with the same key; `LowestLatency` picks the lowest-p50 instance.
- **Health-aware filtering** — instances marked unhealthy are excluded from routing; recovery puts them back after `health_recovery_ms`.
- **Per-(caller, service) concurrency** — one slow caller can't starve others; cap fires per-caller, not globally.
- **Migration interaction** — when a daemon migrates (compute layer), its serving capacity follows; in-flight RPCs get `Cancelled` rather than disappearing silently.

### Phase 3
- **Server-streaming order** — N response packets arrive in order; cancellation mid-stream cleanly closes both ends.
- **Trace context propagation** — `traceparent` / `tracestate` headers round-trip; hedged retries share `trace_id` but emit fresh `span_id`s.
- **Hedged requests** — hedge of N=2 wins on the faster reply, cancels the loser.
- **Circuit breaker** — `OPEN` state short-circuits calls; `HALF_OPEN` probes; `CLOSED` resumes; transitions match the documented state machine.

## Out of scope

- **Pub/sub replacement** — nRPC complements channels, doesn't replace them. Event-bus use cases stay on channels.
- **Service-mesh sidecar** — nRPC runs in-process. There's no plan for a separate sidecar binary that intercepts traffic à la Istio / Linkerd.
- **Mutual TLS / cert rotation** — Net's existing AEAD + capability-token model is the auth substrate; nRPC inherits it. We're not bolting an alternative on.
- **Schema-validated payloads in v1** — payloads are `Bytes`. Optional schema registry is a Phase 4 follow-up; until then, application code owns serde.
- **Sync RPC** — every API is async-only; there is no blocking `call_blocking` shape. Bindings (Python especially) wrap the async runtime in their idiomatic way.

## Open design questions

These are the calls I want sanity-checked before code lands:

1. **UUIDv7 vs `(u64, u64)` for `request_id`** — UUIDv7 is sortable and avoids the per-(caller, target) collision-tracking burden. Cost is 16 B per packet vs. 8 B if we used a counter. Recommend UUIDv7 unless wire size is at a premium.

2. **Per-call vs per-stream framing** — putting RPC on its own packet kinds (this plan) keeps the wire clean. Putting RPC on top of a "system" channel reuses existing publish/subscribe wire and avoids a wire bump. The first composes better with future server-streaming (each response is its own packet, naturally), so I'd accept the wire bump.

3. **`RpcStatus` numbering** — should `0x0001..0x7FFF` mirror gRPC status codes (NotFound = 5, etc.) for operator familiarity, or be a Net-native enumeration? Mirroring eases interop with existing tooling but locks us into gRPC's shape forever. Recommend Net-native with documented gRPC-equivalence in the doc, not at the wire level.

4. **`CallOptions::deadline` as `Instant` vs `Duration`** — `Instant` is unambiguous about clock semantics but doesn't encode well across the wire (the server's `Instant` is incomparable). The wire carries `unix_nanos`; the API takes `Instant` and converts at the call boundary. Acknowledge both clock skew and the fact that "deadline" is a hint to the server, not a contract — server may complete after the deadline if it didn't observe cancellation in time.

5. **Hedging in v1?** — implementing it requires the cancellation machinery to be airtight (the loser's response must be discarded cleanly). Phase 3 is the right home, but if we want hedging earlier, Phase 1 needs solid cancellation tests as a prerequisite. Plan above keeps hedging in Phase 3.
