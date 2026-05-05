# nRPC — request/response as a CortEX fold convention

Plan for a first-class request/response primitive on Net. **Architectural anchor: nRPC is not a new subsystem. It is a convention layer over CortEX folds plus one missing channel-layer primitive.** Every piece of the request/response state machine — correlation, idempotency, snapshot/restore, replay-debugging, causal-chain integration, capability-token authz — already exists in CortEX with different names. nRPC is a typed `dispatch` enum on `EventMeta`, a channel-naming convention, and small caller-side / server-side helpers.

## Build status

What's landed in-tree (post-Phase-1 prerequisites):

- ✅ **`SubscriptionMode::QueueGroup`** on the channel roster (`channel/roster.rs`) — work-distribution dispatch alongside the existing `Broadcast` mode. `add_with_mode` / `dispatch_recipients` / `subscriber_mode` API; back-compat shims preserve every existing call site. 8 regression tests.
- ✅ **`MembershipMsg::Subscribe.queue_group: Option<String>`** wire field (`channel/membership.rs`) — `u8` length + UTF-8 bytes after the existing token field. Forward-compat: pre-queue-group senders (zero remaining bytes after token) decode as `Broadcast`. 5 regression tests.
- ✅ **`Mesh::subscribe_channel_in_queue_group[_with_token]`** public APIs and the inbound-Subscribe handler routes mode through to `roster.add_with_mode`. The publisher (`mesh.rs:5164`) consumes `dispatch_recipients` instead of `members`, so queue-group subscribers actually load-balance.
- ✅ **`cortex::rpc` codec** (`cortex/rpc.rs`) — dispatch constants (`DISPATCH_RPC_REQUEST/RESPONSE/CANCEL/DEADLINE_EXCEEDED`), flag bits (`FLAG_RPC_IDEMPOTENT/STREAMING_RESPONSE/PROPAGATE_TRACE`), `RpcStatus` enum (Net-native with documented gRPC equivalence), `RpcRequestPayload` / `RpcResponsePayload` round-trip codec with `MAX_RPC_*` caps. 15 regression tests pin wire stability + decode-rejection of malformed payloads.
- ✅ **`RpcServerFold`** (`cortex/rpc.rs`) — `RedexFold<()>` that decodes REQUEST events, dispatches the handler in tokio, emits RESPONSE via a `RpcResponseEmitter` callback. `RpcCancellationToken` (Notify+AtomicBool wrapper, race-safe), `RpcContext` (caller_origin + decoded payload + cancellation), `RpcHandler` async-trait, `RpcHandlerError` (Application/Internal). Handler panic caught via `catch_unwind` and surfaced as `RpcStatus::Internal`. Fast deadline-already-passed short-circuit. CANCEL flips the in-flight token. Malformed payloads emit `UnknownVersion` and continue (do not kill the cortex adapter). 10 regression tests.
- ✅ **`RpcClientFold`** + **`RpcClientPending`** (`cortex/rpc.rs`) — symmetric caller side. `RpcClientPending::register(call_id) -> oneshot::Receiver`; the fold's `apply` decodes RESPONSE events and routes them to the matching pending sender. Re-register of the same call_id closes the prior receiver (misuse detection). 5 regression tests.
- ✅ **End-to-end loopback integration test** (`tests/integration_nrpc_loopback.rs`) — proves the server + client folds compose into a working request/response round trip without going through the real Mesh publish path (uses synthesized `RedexEvent`s). 6 tests: round-trip, multiplexed concurrent calls, exactly-once handler invocation, cancellation flowing into the handler, application error round-trip, panic surfacing as Internal.
- ✅ **Per-channel-hash inbound dispatch hook on `MeshNode`** — `register_rpc_inbound(channel_hash, dispatcher)` / `unregister_rpc_inbound(channel_hash)` API. The mesh's inbound packet path consults the dispatcher map per packet (one DashMap get); registered channel hashes route directly to the dispatcher and skip the per-shard `inbound` queue. **Wire-format change**: the publish path (`publish_to_peer`) now stamps `channel_hash` on the outgoing packet header (was always `0` pre-fix). New `ThreadLocalPooledBuilder::set_channel_hash` exposes the underlying builder method. End-to-end network test (`tests/nrpc_inbound_dispatcher.rs`, 3 tests) proves: register/unregister round-trip, registered dispatchers receive published events through real network, unregister restores the shard-inbound path.
- ✅ **`Mesh::serve_rpc(service, handler)` / `Mesh::call(target_node_id, service, payload, opts)` glue** (`adapter/net/mesh_rpc.rs`). The wire-up:
  - **`serve_rpc`** registers an inbound dispatcher for `<service>.requests`'s channel hash. The dispatcher pushes events into a tokio mpsc; a bridge task drains it and runs each event through the `RpcServerFold` (which spawns the handler). The fold's emit closure publishes RESPONSE events on `<service>.replies.<caller_origin>` via the standard roster-based `Mesh::publish` (which works because the caller pre-subscribes to its reply channel from the server).
  - **`call`** lazy-subscribes the caller to its own reply channel from `target_node_id` (one round-trip per (target, service) pair, cached). Allocates a `call_id`, registers a oneshot in the per-Mesh `RpcClientPending`, **direct-sends** the REQUEST to `target_node_id` via `publish_to_peer` (now `pub(super)`) bypassing the local subscriber roster — RPC's caller-knows-target model doesn't fit the publisher-led pub/sub roster. Awaits the receiver; deadline timer fires CANCEL on timeout. Returns `RpcReply` on `Ok`, `RpcError` on any failure.
  - **`ServeHandle`** (RAII) unregisters the dispatcher and aborts the bridge task on Drop.
  - **Per-Mesh state additions** on `MeshNode`: `rpc_client_pending: Arc<RpcClientPending>`, `rpc_next_call_id: Arc<AtomicU64>`, `rpc_reply_subscriptions: Arc<Mutex<Vec<(u64, String)>>>`. All initialized in the constructor, exposed via `pub(super)` accessors.
- ✅ **End-to-end Mesh integration test** (`tests/integration_nrpc_mesh.rs`, 4 tests through real network handshake): round-trip echo, multiple sequential calls reusing the lazy reply subscription with exactly-once handler invocation, server panic surfaces as `Internal`, deadline emits CANCEL and surfaces as `Timeout` to the caller.
- ✅ **Real-network queue-group coverage** (`tests/queue_group_dispatch.rs`, 2 tests): two `QueueGroup` subscribers on different nodes divide a stream of 100 events between them with exactly-once delivery; broadcast subscriber + queue-group pool coexist on one channel ("audit logger + worker pool" pattern from the design doc).
- ✅ **Phase 2 first chunk: service discovery via capability announcements**. `Mesh::serve_rpc` auto-registers the service in a per-Mesh `rpc_local_services` set; `announce_capabilities[_with]` auto-merges `nrpc:<service>` tags onto the announced `CapabilitySet`, propagating through the existing capability-broadcast machinery. Two new public APIs:
  - `Mesh::find_service_nodes(service) -> Vec<u64>` queries the local capability index for nodes carrying the `nrpc:<service>` tag.
  - `Mesh::call_service(service, payload, opts) -> Result<RpcReply, RpcError>` shortcut: finds candidates, picks one via naive `call_id %  len()` round-robin, dispatches via the existing direct-addressed `call(target, ...)`. Returns `RpcError::NoRoute` if no servers advertise the tag.
  
  `ServeHandle::Drop` removes the service from the local registry so subsequent announcements stop emitting the tag.
- ✅ **Phase 2 end-to-end test** (`tests/integration_nrpc_service_discovery.rs`, 4 tests): three nodes, two serve "echo", one caller uses `call_service` — `call_service_discovers_servers_via_capability_announcements` asserts both servers are exercised by round-robin distribution; `sticky_routing_pins_a_key_to_one_server` pins consistency under `RoutingPolicy::Sticky`; `random_routing_distributes_across_servers` validates `RoutingPolicy::Random`; `call_service_with_no_servers_returns_no_route` returns `RpcError::NoRoute` with a diagnostic naming the missing tag.
- ✅ **`RoutingPolicy` enum** plumbed onto `CallOptions` (default `RoundRobin`):
  - `RoundRobin` — naive `call_id % len`. Even distribution.
  - `Random` — xxh3 of `call_id`, modulo. Stateless, even.
  - `Sticky { key: u64 }` — xxh3 of key, modulo a sorted candidate list. Same key → same target while the candidate set is stable. Useful for session affinity / shard routing / conversation pinning.
  - `LowestLatency` — picks the candidate with smallest `latency_us` per the local `ProximityGraph`. Candidates with no proximity entry fall to `u64::MAX` and lose; if every candidate lacks proximity data, falls back deterministically to the lexicographically-first sorted node id (so a freshly-discovered service still routes consistently).
- ✅ **`filter_unhealthy: bool` on `CallOptions`** (default `true`) — skips candidates whose `ProximityGraph` entry reports `!is_available()` (i.e. `Unhealthy` / `Unknown`). Pin: candidates with NO proximity entry are KEPT (absence of evidence ≠ evidence of unhealth), so a freshly-announced server isn't falsely filtered just because pingwaves haven't propagated yet.
- ✅ **EntityId ↔ node_id bridge** — `MeshNode::entity_id_for_node(u64) -> Option<[u8; 32]>` accessor consults `peer_entity_ids` to map session-layer node ids to entity-layer keys. This is the single piece that was missing; `LowestLatency` and `filter_unhealthy` both flow through it.
- ✅ **Two new bridge tests** in `tests/integration_nrpc_service_discovery.rs`:
  - `lowest_latency_falls_back_to_first_when_no_proximity_data` — 20 calls under `LowestLatency` with no pingwaves exchanged. All 20 land on the lexicographically-first sorted candidate (deterministic fallback).
  - `filter_unhealthy_keeps_candidates_with_no_proximity_data` — 20 calls with `filter_unhealthy=true` against two fresh servers (no proximity data); both servers receive a non-zero share. Pins the "absence of evidence ≠ unhealth" semantic.

Phase 1 + Phase 2 are functionally complete. The asymmetric routing pattern (REQUESTs direct-unicast, RESPONSEs roster-based) is what Phase 1 settled on and remains in Phase 2 — the discovery layer just removes the need for the caller to specify `target_node_id` explicitly, and the four routing policies + health filter let the caller hint at session affinity, even distribution, latency-driven selection, or unhealthy exclusion.

- ✅ **Rust SDK typed wrappers** (`sdk/src/mesh_rpc.rs`):
  - **Raw passthroughs** (`Mesh::serve_rpc`, `Mesh::call`, `Mesh::call_service`, `Mesh::find_service_nodes`) — thin delegates to the underlying `MeshNode` API.
  - **Typed wrappers** (`Mesh::serve_rpc_typed`, `Mesh::call_typed`, `Mesh::call_service_typed`) — auto serde via a per-call selectable `Codec` (default `Json`, `JsonPretty` for diagnostic dumps). The handler signature is `Fn(Req) -> Future<Output = Result<Resp, String>>` — `Err(String)` surfaces as `RpcError::ServerError` with `RpcStatus::Application(0x4001)` and the message as the body. Malformed request bodies short-circuit to `Application(0x4000)` before the user closure runs.
  - **`Codec` enum** with `encode<T>` / `decode<T>` helpers; round-trips primitive and struct types via `serde_json`.
  - **Re-exports** of `RpcError`, `RpcReply`, `CallOptions`, `RoutingPolicy`, `ServeHandle`, `RpcContext`, `RpcHandler`, `RpcHandlerError`, `RpcStatus`, `ServeError` from the SDK so users have one place to import from.
  - **4 unit tests** (`sdk/tests/mesh_rpc_typed.rs`) pinning the typed-handler trait round-trip, application-error mapping, malformed-body short-circuit (user closure NOT invoked), and codec round-trip semantics.

- ✅ **`ChannelConfigRegistry` prefix-match** — new `insert_prefix(prefix, config)` / `remove_prefix(prefix)` API. `get_by_name(name)` falls back to a prefix walk when no exact match exists; the first prefix `name` starts with wins. The exact-match hot path (DashMap get) is unaffected; prefix lookups are O(num_prefixes) on the slow path. Documented as "use sparingly — one prefix per service is fine, hundreds is not."
- ✅ **SDK auto-registration** in `Mesh::serve_rpc` and `Mesh::serve_rpc_typed` — registers two `ChannelConfig` entries per service:
  - Exact: `<service>.requests` (channel callers publish REQUESTs onto).
  - Prefix: `<service>.replies.` (admits every per-caller `<service>.replies.<caller_origin>` subscribe without pre-registration).
  Both default to permissive (no `publish_caps`, no `require_token`); operators who want RPC ACLs can call `register_channel` / `register_channel_prefix` themselves before `serve_rpc` to override. Resolves the SDK channel-registry friction noted in the prior follow-up.
- ✅ **End-to-end SDK nRPC tests** in `sdk/tests/mesh_rpc_typed.rs` (4 tests, real network handshake): typed `call_typed` round-trip, handler `Err(String)` mapping, `call_service_typed` discovers the server via capability announcements, codec round-trip semantics. **All four tests pass over the SDK's default `MeshBuilder::build` path** — no special opt-in required.
- ✅ **W3C Trace Context propagation** (`cortex::rpc::TraceContext` + `extract_trace_context` / `build_trace_headers` helpers). New `CallOptions::trace_context: Option<TraceContext>` and `RpcContext::trace_context: Option<TraceContext>` fields. When the caller sets `CallOptions::trace_context`, the SDK emits `traceparent` / `tracestate` headers and sets `FLAG_RPC_PROPAGATE_TRACE`; the server's fold extracts the headers and populates `RpcContext::trace_context`. nRPC is **transport-only** — application code on both sides reads/writes via whatever tracing backend it has wired up (`tracing-opentelemetry`, Datadog, etc.). Empty `tracestate` is omitted on the wire (W3C convention). 4 unit tests + 1 end-to-end test (`integration_nrpc_mesh::rpc_trace_context_propagates_to_server`) prove the round-trip via real network publish.
- ✅ **Phase 3 first chunk: streaming responses.** Multi-fire `DISPATCH_RPC_RESPONSE` events for one `call_id` marked non-terminal vs. terminal via the `nrpc-streaming` header (`continue` / `end`). New surface:
  - **Wire markers**: `HEADER_NRPC_STREAMING` header with `continue` (non-terminal) and `end` (terminal-Ok) values; non-`Ok` status is implicitly terminal regardless of header. `classify_streaming_chunk(&resp) -> StreamingChunkKind` is the single decision point. Caller sets `FLAG_RPC_STREAMING_RESPONSE` on the REQUEST to signal "expect multi-fire".
  - **Server side**: `RpcResponseSink` (unbounded mpsc, `sink.send(body)` is non-blocking), `RpcStreamingHandler` async-trait taking `(ctx, sink)`, and `RpcServerStreamingFold` (parallel to `RpcServerFold` but spawns a pump task draining the sink and emitting per-chunk `nrpc-streaming: continue` frames; handler return → terminal `end` frame, handler `Err` → terminal non-`Ok` frame, handler panic caught by `catch_unwind` → terminal `Internal`). `Mesh::serve_rpc_streaming` is the public glue.
  - **Per-call ordering guarantee**: the streaming fold takes an `RpcAsyncResponseEmitter` (Arc<dyn Fn(...) -> BoxFuture<()>>) instead of the unary fold's sync `RpcResponseEmitter`, and the pump task `.await`s each emit before reading the next sink chunk. Without this, two chunks emitted in tight succession would race into the publish path via independent `tokio::spawn`s and arrive at the caller out of order — or be eclipsed by the terminal frame and lost entirely (caller stops reading once terminal arrives). Pinned by the SDK streaming test (the unary fold keeps the cheaper sync emitter — exactly one RESPONSE per call, no ordering dependency).
  - **Client side**: `RpcClientPending` refactored from oneshot-only to a `PendingEntry::{Unary | Streaming}` enum so a single `RpcClientFold` demuxes both call kinds. `register_streaming(call_id) -> mpsc::UnboundedReceiver<StreamItem>` is the streaming counterpart of `register`. `Mesh::call_streaming` returns an `RpcStream: futures::Stream<Item = Result<Bytes, RpcError>>`; terminal-Ok closes the stream, terminal-error yields one final `Err(RpcError::ServerError)` then closes. `RpcStream::Drop` clears the pending entry and best-effort emits CANCEL via direct unicast so the server's handler observes `ctx.cancellation`.
  - **Tests** (`tests/integration_nrpc_streaming.rs`, 3 tests through real network): `rpc_streaming_collects_all_chunks` (server emits 5 chunks, caller collects all 5 in order, sees clean EOF), `rpc_streaming_drop_cancels_handler` (caller drops mid-stream, handler observes `ctx.cancellation` cooperatively), `rpc_streaming_terminal_error_after_partial_stream` (server emits 2 chunks then `Err` → caller sees both chunks then `RpcError::ServerError` with `Internal` status).
- ✅ **SDK typed streaming surface** (`sdk/src/mesh_rpc.rs`):
  - **Raw passthroughs** (`Mesh::serve_rpc_streaming`, `Mesh::call_streaming`) — thin delegates plus the same `auto_register_rpc_channels` as the unary path.
  - **Typed wrappers** (`Mesh::serve_rpc_streaming_typed`, `Mesh::call_streaming_typed`) auto serde via the per-call `Codec`. Handler signature is `Fn(Req, ResponseSinkTyped<Resp>) -> Future<Output = Result<(), String>>`. `Err(String)` surfaces as `RpcError::ServerError` with `RpcStatus::Application(0x4001)` carrying the message; malformed request bodies short-circuit to `Application(0x4000)` before the user closure runs.
  - **`ResponseSinkTyped<Resp>`** wraps `RpcResponseSink` + `Codec` and `send(&value) -> Result<(), String>` encodes per send (encode-failure surfaced to the handler so it can choose to abort the stream).
  - **`RpcStreamTyped<Resp>`** wraps `RpcStream` and decodes each chunk; decode failure terminates the stream with one `Err(RpcError::ServerError(Internal))` carrying the decode diagnostic.
  - **Re-exports** of `RpcResponseSink`, `RpcStreamingHandler`, `StreamItem`, `RpcStream` from the SDK module.
  - **3 unit tests** (`sdk/tests/mesh_rpc_streaming_typed.rs`, all over real network handshake): `typed_streaming_collects_all_chunks`, `typed_streaming_handler_error_after_partial_stream`, `typed_streaming_chunk_decode_failure_terminates_stream` (server emits a JSON shape the caller's `Resp` can't decode → caller sees one `Err` then EOF, never silently swallows).
- ✅ **Phase 3 caller-side resilience: retry helper** (`sdk/src/mesh_rpc_resilience.rs`):
  - **`RetryPolicy`** with full-half jitter (each backoff scaled by uniform random in `[0.5, 1.0]`), exponential growth (`backoff_multiplier`, default `2.0`), upper-bound cap (`max_backoff`), and a swappable `retryable: Arc<dyn Fn(&RpcError) -> bool>` predicate. Default policy: 3 attempts, 50ms initial → 1s cap.
  - **`default_retryable`** — retries `Timeout`, `Transport`, and `ServerError` for canonical transient statuses (`Internal`, `Backpressure`, server-observed `Timeout`); does NOT retry `NoRoute`, application errors, `NotFound`, `Unauthorized`, `UnknownVersion`, or `Cancelled` (caller-fixable / terminal).
  - **Four wrappers on `Mesh`**: `call_with_retry`, `call_service_with_retry`, `call_typed_with_retry`, `call_service_typed_with_retry`. Typed variants encode once and reuse the bytes across attempts; service variants re-resolve the candidate set per attempt so failover is automatic.
  - **No new dependencies** — jitter source is a tiny inline mix of `SystemTime::now()` nanos with the attempt counter (good enough to decorrelate retry storms; the goal is not unpredictability, it is independence across callers).
  - **4 tests** (`sdk/tests/mesh_rpc_retry.rs`): `retry_eventually_succeeds_after_transient_failures` (server fails first 2, succeeds 3rd; wrapper observes single `Ok` reply and exactly 3 handler invocations), `retry_does_not_retry_application_errors` (typed handler `Err(String)` surfaces as `Application(0x4001)` after 1 attempt — no retry), `retry_exhaustion_surfaces_last_error` (3-attempt cap, server always Internal, last error round-trips with original diagnostic), `default_retryable_classifies_canonical_errors` (pure-function unit pin on the predicate's classification).
- ✅ **Phase 3 caller-side resilience: hedge helper** (`sdk/src/mesh_rpc_resilience.rs`):
  - **`HedgePolicy { delay, hedges }`** — fire-then-race: primary at `t=0`, additional hedges at `t=delay*idx`, first reply (Ok or Err) wins; if first finisher is `Err`, the wrapper waits for remaining hedges before surfacing the last error. Defaults: 50ms delay, 1 hedge.
  - **Four wrappers on `Mesh`**: `call_with_hedge_to(targets, ...)` / `call_typed_with_hedge_to` for explicit-target hedging (e.g. primary + warm-standby), `call_service_with_hedge` / `call_service_typed_with_hedge` for capability-index-driven hedging across replicas. Service variants sort the candidate set so the prefix taken is deterministic.
  - **Why service-only and explicit-targets-only, not direct-to-one-target**: hedging to the same target is always wrong (same backlog, same GC pause, doubles your load for nothing). Hedging only buys p99 reduction across distinct replicas / endpoints.
  - **Why no `filter_unhealthy` on the service variant**: hedge's whole premise is "be robust to per-node slowness" — including unhealthy-but-still-responsive nodes. Filtering them out reduces the redundancy hedging buys you. Documented; users who want health-aware single-target dispatch use `call_service` with a routing policy directly.
  - **Cancellation tradeoff**: loser hedges are NOT explicitly CANCELled today. The losing futures are dropped on the caller side (so the caller doesn't await them), but the server-side handlers run to completion and their replies are silently discarded on arrival. Bandwidth + server-CPU is paid; correctness preserved. Documented as a known tradeoff; a future enhancement will wire CANCEL emission into the unary call's drop path (the streaming path already has it via `RpcStream::Drop`).
  - **3 tests** (`sdk/tests/mesh_rpc_hedge.rs`): `hedge_backup_wins_when_primary_is_slow` (primary sleeps 800ms, backup is instant; with 50ms hedge delay the backup's body wins under 600ms wall-clock), `hedge_zero_degrades_to_single_call` (`hedges=0` falls back to a single straight call against `targets[0]`; second target ignored), `hedge_empty_targets_returns_no_route` (empty `targets` slice → immediate `RpcError::NoRoute` with diagnostic).
- ✅ **Phase 3 caller-side resilience: circuit breaker** (`sdk/src/mesh_rpc_resilience.rs`):
  - **`CircuitBreaker`** with `CircuitBreakerConfig` — three-state machine `Closed → Open → HalfOpen → Closed/Open`. Defaults: 5 consecutive failures to trip, 30s open cooldown, 1 successful probe to close.
  - **Different shape from retry/hedge**: a long-lived stateful guard the user instantiates once (typically per logical downstream — one per service, or one per `(service, target)` pair) and shares via `Arc<CircuitBreaker>`. The wrapper takes a closure: `breaker.call(|| async { mesh.call_typed::<Req,Resp>(...).await }).await`. Generic over the inner result type so it composes around raw, typed, retried, OR hedged calls without specialized variants.
  - **`BreakerError::{Open | Inner(RpcError)}`** — pattern-match `Open` to fall back, `Inner` to handle the underlying error. `into_rpc_error()` flattens to `RpcError` for callers that don't care about the distinction.
  - **`default_breaker_failure`** predicate matches `default_retryable` — counts transient infra failures (`Timeout`, `Transport`, server `Internal` / `Backpressure` / `Timeout`); does NOT count application errors (caller-fixable bugs aren't health signals). Swap via `CircuitBreakerConfig::failure_predicate`.
  - **HalfOpen semantics**: at most ONE concurrent probe; other calls during HalfOpen short-circuit. Probe success → `consecutive_successes++` (transition to `Closed` when `success_threshold` met); probe failure → straight back to `Open` with cooldown reset.
  - **Observability**: `state()` and `consecutive_failures()` are cheap snapshots; `reset()` is the operator override (clear all state to `Closed`, zero counters) for runbook scenarios.
  - **No new dependencies** — state lives in `std::sync::Mutex` (held briefly, never across `await`).
  - **5 tests** (`sdk/tests/mesh_rpc_breaker.rs`, real-network where state transitions matter): `breaker_full_state_machine_cycle` (3 failures trip → 5 short-circuit → fix downstream + wait cooldown → probe closes → normal flow resumes; pinned via handler-invocation counter that the cooldown phase never invokes the server), `breaker_failed_half_open_probe_reopens` (bad probe re-opens with fresh cooldown, doesn't slip into Closed), `breaker_application_errors_do_not_trip` (5 consecutive typed `Err(String)` keeps state Closed, counter at 0), `breaker_reset_clears_state`, `breaker_error_flatten`.

What's still pending:

- ⏳ **`Mesh::serve_rpc(service, handler)` / `Mesh::call(service, payload, opts)` glue**. The shape is locked; the implementation is the next concrete pickup.

  **Seam decision (locked): Option A — cortex adapter on the request channel.** `serve_rpc` opens a real `CortexAdapter` on `<service>.requests` with the `RpcServerFold` as its fold. Subscribed inbound events (delivered via the existing `inbound: DashMap<u16, SegQueue<StoredEvent>>` path) are ingested into the adapter's local redex log; the adapter's tail-and-fold task drives `RpcServerFold::apply` from there. The alternative (Option B — bypass the redex log, feed subscribed events directly into the fold via a tokio channel) was rejected for Phase 1 because (a) it requires a hot-path code change to the mesh's inbound delivery point, and (b) Option A reuses every existing piece of plumbing while gaining durability + replay + snapshot-restore of in-flight RPC state for free. The per-call redex-append latency cost (~microseconds) is acceptable when the network alone is hundreds of microseconds.

  Concrete steps:

  1. **`serve_rpc(service, handler)`**:
     - Open `CortexAdapter::open(redex, "<service>.requests", ..., RpcServerFold::new(handler, emit), ())`.
     - Where `emit: RpcResponseEmitter` builds an `EventEnvelope` (meta + `RpcResponsePayload::encode()`) and calls `Mesh::publish(channel_publisher_for("<service>.replies.<caller_origin>"), payload)`. Reply-channel naming uses the caller's `origin_hash` (8-byte hex), so each caller's reply channel is private and naturally subscribed only by them.
     - Self-subscribe via `Mesh::subscribe_channel_in_queue_group(self_node_id, "<service>.requests", "<service>")`. This is a local-only roster mutation; the queue-group dispatch then routes one-of-N requests across replicas that all subscribe with the same group name.
     - Bridge inbound events into the cortex adapter's ingest path. The mesh already pushes subscribed events into `inbound[shard_id]`. The bridge spawns a task that polls `MeshNode::poll_shard(...)` for the relevant shard, filters to events for the channel, and calls `adapter.ingest(envelope)`. (A future optimization: hook directly at the inbound-delivery point to skip the poll loop. Phase 2 work.)
     - Return a `ServeHandle` whose Drop closes the adapter, unsubscribes, and stops the bridge task.

  2. **`call(service, payload, opts)`**:
     - Lazily ensure: (a) a subscription to `<service>.replies.<self_origin>` (Broadcast mode, sole subscriber by construction), (b) a `CortexAdapter` on that channel with `RpcClientFold` as its fold, (c) the same inbound bridge as above.
     - Allocate `call_id` from a per-Mesh `AtomicU64`.
     - `pending.register(call_id) -> oneshot::Receiver`.
     - Publish REQUEST envelope on `<service>.requests` via `Mesh::publish`.
     - Await the receiver under `opts.deadline` (race with `tokio::time::sleep_until`).
     - On future-drop OR deadline-fire: publish CANCEL envelope on `<service>.requests` and `pending.cancel(call_id)`.

  3. **Per-Mesh state** (a small extension to `MeshNode`):
     - `rpc_servers: DashMap<String /* service */, ServeHandle>` — active server registrations.
     - `rpc_client_pending: Arc<RpcClientPending>` — the singleton pending-calls store.
     - `rpc_next_call_id: AtomicU64`.
     - `rpc_reply_subscription: Mutex<Option<ReplySubscription>>` — lazily initialized on first `call`, torn down after `idle_reply_subscription_ttl` of no in-flight calls.

  4. **End-to-end integration test** (once the glue lands): two `MeshNode` instances in one process. Node A: `serve_rpc("echo", echo_handler)`. Node B: `call("echo", b"hi")`. Assert round-trip + queue-group load distribution across N>1 servers + cancellation + crash-recovery (kill node A mid-call, restart with the same redex; the request gets re-folded on rehydrate and the response lands).
- ⏳ **End-to-end integration test against real Mesh instances** — once the glue lands, two Mesh nodes in one process: one calls `serve_rpc("echo", ...)`, the other calls `call("echo", ...)`; assert round-trip + queue-group load distribution across N servers + cancellation flowing across the network.
- ⏳ **Phase 2** — service registry derived from existing capability announcements; routing policies; SDK typed wrappers for the four bindings.
- ⏳ **Phase 3** — streaming responses, tracing context propagation, retry/circuit-breaker/hedging helpers.

## The framing

An RPC server is a CortEX fold:

| RPC concept | CortEX equivalent |
|------|------|
| Server's accumulated state | `RedexFold::State` |
| A request | An `EventEnvelope` with `meta.dispatch = REQUEST` |
| Correlation ID | `EventMeta::seq_or_ts` (per-caller monotonic) |
| Caller identity | `EventMeta::origin_hash` (AEAD-verified upstream) |
| Response | An `EventEnvelope` with `meta.dispatch = RESPONSE`, same `seq_or_ts` |
| Awaiting a response | `wait_for_seq(call_id)` on the reply channel |
| Snapshot of in-flight RPC state | `CortexAdapter::snapshot()` |
| Mid-call crash recovery | Replay from log (the request was durable before processing) |
| Idempotency | Fold's natural state — replaying the same `seq_or_ts` is a no-op |
| Cancellation | A `CANCEL` event with the request's `seq_or_ts` |
| Distributed tracing | `FLAG_CAUSAL` + the existing causal-chain integration |
| Service authorization | Channel-level capability tokens (existing) |
| Replay debugging | "Which request caused the bad state" — replay the channel |

This is the same pattern that drives event-sourcing and CQRS architectures. We get all of it for the price of a `dispatch` enum extension.

## The one missing primitive: `SubscriptionMode::QueueGroup`

Channels today broadcast every published event to every subscriber. That's correct for events but wrong for request/response: N replica servers each running the request and racing on the reply is wasteful work and a synchronization headache (which response is canonical?). RPC needs **work-distribution semantics**: one-of-N delivery to a named group of co-equal subscribers.

JetStream / NATS / SQS all settled on the same shape. We adopt it:

```rust
pub enum SubscriptionMode {
    /// Existing behavior: every published event is delivered to
    /// this subscriber. Multiple subscribers in this mode receive
    /// independent copies. Right for events.
    Broadcast,

    /// Work-distribution: every published event is delivered to
    /// exactly one subscriber in the named group. Multiple
    /// subscribers in the same `QueueGroup(name)` divide the
    /// stream amongst themselves. Right for request/response.
    QueueGroup(String),
}
```

A subscriber's mode is set at `subscribe` time and is stable for the lifetime of the subscription. The roster bookkeeping changes from `subscribers: HashSet<EntityId>` to `subscribers: HashMap<EntityId, SubscriptionMode>`; the dispatch path picks one queue-group member (round-robin or P2C) per event, and broadcasts to all `Broadcast` subscribers.

This primitive is useful beyond RPC: any work-queue pattern (background job processing, ETL pipeline shards, batched fetchers) wants the same shape today. RPC is the forcing function but the surface is general.

## What's already there (that we don't have to build)

- **Typed dispatch on `EventMeta`** — `dispatch: u8` with `0x00..0x7F` reserved for CortEX-internal and `0x80..0xFF` for application/vendor. nRPC consumes a small block of the cortex-internal range.
- **Per-event integrity** — `compute_checksum_with_meta` covers the meta header (audit #8), so a bit-flip in `dispatch: REQUEST → RESPONSE` is detected by the per-event check.
- **Per-origin monotonic counters** — `seq_or_ts` is documented as either per-origin monotonic OR unix nanos. RPC uses per-caller monotonic; that's the deterministic-fold-order option, no extra work.
- **`wait_for_seq` futures** — `CortexAdapter::wait_for_seq(seq).await` returns when the fold has applied `seq`. This is literally the response-await primitive.
- **Snapshot / restore** — `applied_through_seq` strict-prefix watermark snapshots cleanly; in-flight RPC state survives restart with the rest of the fold's state.
- **Causal chain** — `FLAG_CAUSAL` events carry a `parent_hash`. RPC requests in the same trace chain together for free.
- **Capability tokens** — `PermissionToken` with `TokenScope::PUBLISH` / `SUBSCRIBE` already gates channel access. Service-level authorization is a small extension (a per-token service allowlist).
- **Mesh-level channel routing** — `SubscriberRoster` + the existing dispatch path already routes published events to remote subscribers across the mesh. No transport changes.
- **Backpressure** — RedEX append is the natural rate-limiter; events that can't be appended fast enough surface to the publisher as a typed error.
- **Identity verification** — `origin_hash` on incoming events is set by the bus from the AEAD-verified peer; not self-claimable.

What's left to build is the *convention layer* on top.

## Conventions

### Channel naming

```text
<service>.requests                       — server(s) subscribe in QueueGroup(<service>)
<service>.replies.<caller_origin_hash>   — caller subscribes in Broadcast (sole subscriber)
```

Caller publishes the request to `<service>.requests`. Exactly one server in the queue group receives it (work-distribution). Server publishes the response to `<service>.replies.<origin_hash>` — a private channel scoped to the caller. Caller is already subscribed (subscription is established lazily on first call to a service and cached for reuse).

This naming matches the existing `ChannelName` shape (forward-slash-separated segments under `cortex::adapter::net::channel`). The reply-channel name encodes the caller's `origin_hash` so each caller subscribes only to their own replies — no cross-caller fan-out.

### `EventMeta::dispatch` values

In the cortex-internal range (`0x00..0x7F`):

```rust
pub const DISPATCH_RPC_REQUEST: u8 = 0x10;
pub const DISPATCH_RPC_RESPONSE: u8 = 0x11;
pub const DISPATCH_RPC_CANCEL: u8 = 0x12;
pub const DISPATCH_RPC_DEADLINE_EXCEEDED: u8 = 0x13;
```

The rest of the dispatch space is unaffected. CortEX adapters that don't care about RPC ignore these dispatches as they ignore any other unknown dispatch.

### Payload shape (after the 24-byte `EventMeta`)

```rust
struct RpcRequestPayload {
    service: String,                    // varint+bytes (max 256)
    deadline_ns: u64,                   // 0 = no deadline
    flags: u16,                         // IDEMPOTENT | STREAMING_RESPONSE | PROPAGATE_TRACE | ...
    headers: Vec<(String, Vec<u8>)>,    // varint count + name/value pairs
    body: Bytes,                        // application-defined
}

struct RpcResponsePayload {
    status: u16,                        // 0x0000 = Ok; see status table
    headers: Vec<(String, Vec<u8>)>,
    body: Bytes,                        // for Ok = app response; for errors = UTF-8 diagnostic
}

struct RpcCancelPayload {}              // empty; the seq_or_ts in EventMeta is the call_id
struct RpcDeadlineExceededPayload {}    // empty; same
```

Encoded with `postcard` for compactness (matches the rest of the cortex envelope conventions).

`status` codes (Net-native, with documented gRPC equivalence in this doc):

| status | meaning | gRPC analog |
|---|---|---|
| `0x0000` | `Ok` | OK |
| `0x0001` | `NotFound` (no service registered with that name) | NOT_FOUND |
| `0x0002` | `Unauthorized` (token doesn't include the requested service) | PERMISSION_DENIED |
| `0x0003` | `Timeout` (server observed deadline expired before starting) | DEADLINE_EXCEEDED |
| `0x0004` | `Backpressure` (server's per-service queue full) | RESOURCE_EXHAUSTED |
| `0x0005` | `Cancelled` (caller emitted CANCEL before server completed) | CANCELLED |
| `0x0006` | `Internal` (handler panicked or returned an error) | INTERNAL |
| `0x0007` | `UnknownVersion` (request payload version not supported) | UNIMPLEMENTED |
| `0x0008..0x7FFF` | reserved | — |
| `0x8000..0xFFFF` | application-defined | — |

### Correlation

`EventMeta::seq_or_ts` is the `call_id`. Caller-generated, per-caller monotonic. Same value on the request, the response, and any associated CANCEL or DEADLINE_EXCEEDED events.

No separate UUID needed — `seq_or_ts` is already 8 bytes and it's already the deterministic-fold-order field.

## The fold pattern

### Server-side: `RpcServerFold`

```rust
pub trait RpcHandler<S>: Send + Sync + 'static {
    type Future: Future<Output = Result<RpcResponsePayload, RpcHandlerError>> + Send;
    fn call(&self, ctx: RpcContext, state: &mut S) -> Self::Future;
}

pub struct RpcServerFold<H, S> {
    handler: H,
    /// Per-caller in-flight set; entries cleared on RESPONSE emission
    /// or on CANCEL / DEADLINE_EXCEEDED.
    in_flight: DashMap<(u64, u64), CancellationToken>, // (origin_hash, call_id) -> token
    /// LRU of completed idempotent calls; key is (origin_hash,
    /// call_id), value is the cached RESPONSE payload. Bounded so a
    /// long-running fold doesn't grow without bound.
    completed_idempotent: lru::LruCache<(u64, u64), RpcResponsePayload>,
    _state: PhantomData<S>,
}

impl<H, S> RedexFold<S> for RpcServerFold<H, S>
where
    H: RpcHandler<S>,
    S: Send + Sync,
{
    fn apply(&mut self, ev: &RedexEvent, state: &mut S) -> Result<(), RedexError> {
        let meta = EventMeta::from_bytes(&ev.payload[..EVENT_META_SIZE])?;
        let key = (meta.origin_hash, meta.seq_or_ts);
        match meta.dispatch {
            DISPATCH_RPC_REQUEST => {
                let req: RpcRequestPayload = postcard::from_bytes(&ev.payload[EVENT_META_SIZE..])?;
                // Idempotency: replay of a previously-completed call
                // returns the cached response without re-running.
                if req.flags & IDEMPOTENT != 0 {
                    if let Some(cached) = self.completed_idempotent.get(&key) {
                        self.emit_response(meta.origin_hash, meta.seq_or_ts, cached.clone());
                        return Ok(());
                    }
                }
                // Fast deadline-already-passed short-circuit: emit
                // Timeout without running the handler.
                if req.deadline_ns != 0 && now_ns() > req.deadline_ns {
                    self.emit_response(meta.origin_hash, meta.seq_or_ts, RpcResponsePayload::timeout());
                    return Ok(());
                }
                let cancel = CancellationToken::new();
                self.in_flight.insert(key, cancel.clone());
                let ctx = RpcContext { caller: meta.origin_hash, call_id: meta.seq_or_ts, request: req, cancel };
                // Spawn the handler off the fold thread. The fold
                // returns immediately so subsequent events
                // (including CANCEL for *this* call_id) can be
                // processed without head-of-line blocking.
                tokio::spawn(self.handler.call(ctx, state));
            }
            DISPATCH_RPC_CANCEL => {
                if let Some((_, token)) = self.in_flight.remove(&key) {
                    token.cancel();
                }
            }
            _ => {} // RESPONSE / DEADLINE_EXCEEDED: ignored on the server side
        }
        Ok(())
    }
}
```

The handler runs in a `tokio::spawn` so the fold doesn't block on application work. When the handler completes, it emits the RESPONSE event via `emit_response`, which publishes to `<service>.replies.<caller_origin_hash>`. The fold sees the RESPONSE indirectly when its `wait_for_seq` future resolves on the reply channel.

Note the head-of-line property: a long-running call doesn't block subsequent calls (or the CANCEL of itself). The fold itself never awaits.

### Caller-side: `RpcClientFold`

```rust
pub struct RpcClientFold {
    /// Pending calls awaiting a response. Each call owns a oneshot
    /// receiver; the fold completes the sender when the matching
    /// RESPONSE arrives.
    pending: DashMap<u64, oneshot::Sender<RpcResponsePayload>>, // call_id -> sender
}

impl RedexFold<()> for RpcClientFold {
    fn apply(&mut self, ev: &RedexEvent, _state: &mut ()) -> Result<(), RedexError> {
        let meta = EventMeta::from_bytes(&ev.payload[..EVENT_META_SIZE])?;
        if meta.dispatch != DISPATCH_RPC_RESPONSE { return Ok(()); }
        let resp: RpcResponsePayload = postcard::from_bytes(&ev.payload[EVENT_META_SIZE..])?;
        if let Some((_, tx)) = self.pending.remove(&meta.seq_or_ts) {
            let _ = tx.send(resp);
        }
        Ok(())
    }
}
```

This fold has empty user state — it's purely a routing index from `call_id` → caller's awaiting future. The actual RPC state is on the server's fold.

## API surface

### Caller: `Mesh::call`

```rust
impl Mesh {
    pub async fn call(
        &self,
        service: &str,
        payload: Bytes,
        opts: CallOptions,
    ) -> Result<RpcReply, RpcError>;
}

#[derive(Debug, Clone, Default)]
pub struct CallOptions {
    pub deadline: Option<Instant>,
    pub idempotent: bool,
    pub trace_context: Option<TraceContext>,
    pub max_in_flight: u32, // caller-side semaphore (default 64)
}
```

Internals: `Mesh::call`
1. Allocates a fresh `call_id` from the per-caller monotonic counter.
2. Registers a oneshot in the local `RpcClientFold::pending` keyed on `call_id`.
3. Publishes a REQUEST event to `<service>.requests` with `meta.seq_or_ts = call_id`, `meta.origin_hash = self.identity.origin_hash()`, `dispatch = DISPATCH_RPC_REQUEST`.
4. Awaits the oneshot. If `opts.deadline` fires first → publishes a CANCEL event (so the server can drop the in-flight entry) and returns `RpcError::Timeout`. If the future is dropped before the response → publishes a CANCEL event.
5. On response: returns the decoded `RpcReply`.

**Subscription**: the first `Mesh::call(service, ...)` lazily subscribes to `<service>.replies.<origin_hash>` in `Broadcast` mode (it's the only subscriber by construction). Subsequent calls reuse the subscription. A background task tears down the reply subscription after `idle_reply_subscription_ttl` of no in-flight calls.

### Server: `Mesh::serve_rpc`

```rust
impl Mesh {
    /// Register a handler for `service`. Subscribes to
    /// `<service>.requests` in QueueGroup(<service>) mode; multiple
    /// nodes calling `serve_rpc` for the same service automatically
    /// form a load-balanced group. Returns a `ServeHandle` whose
    /// Drop deregisters and unsubscribes.
    pub fn serve_rpc<S, H>(
        &self,
        service: &str,
        initial_state: S,
        handler: H,
    ) -> Result<ServeHandle, ServeError>
    where
        S: Send + Sync + 'static,
        H: RpcHandler<S>;
}
```

Internals: `serve_rpc` opens a CortEX adapter on `<service>.requests` with an `RpcServerFold` wrapping the user's handler. The adapter subscribes to the channel in `QueueGroup(<service>)` mode. The `ServeHandle` carries a `Drop` that closes the adapter and unsubscribes.

Multi-instance is automatic: every node that calls `serve_rpc("foo", ...)` joins the `foo` queue group. The channel layer's queue-group dispatch picks one of them per request.

### SDK typed wrapper

```rust
impl RpcClient {
    pub async fn call<Req, Resp>(
        &self,
        service: &str,
        request: &Req,
    ) -> Result<Resp, RpcError>
    where
        Req: Serialize,
        Resp: DeserializeOwned;
}
```

Codec selectable per client (`serde_json` / `postcard`). Bindings (Node / Python / Go) get parallel typed surfaces.

## Service discovery (Phase 2)

Reuses the existing `CapabilityAnnouncement` machinery. Each receiving node already learns "node X subscribes to channel Y" from announcements; we just add a small derived index:

```rust
pub struct ServiceRegistry {
    /// service_name -> nodes serving it (derived from
    /// CapabilityAnnouncement subscriptions to <service>.requests)
    services: DashMap<String, BTreeSet<EntityId>>,
}
```

The registry is populated automatically from existing channel announcements — no new wire kind. A `Mesh::call_service(name, ...)` shortcut consults the registry to confirm at least one server is reachable; the actual routing decision (which of N servers) happens at the channel layer via the queue-group dispatcher.

For routing-policy pluggability (round-robin, P2C, sticky, lowest-latency), the policy is configured per `serve_rpc` call (server-side) AND per `Mesh::call` (caller can hint). The default is P2C against in-flight count, which is what `behavior::loadbalance.rs` already implements.

## Authorization

Two layers, both load-bearing:

1. **Channel-level (existing).** Capability tokens already gate `subscribe` and `publish` per channel. Calling an RPC service requires `publish` on `<service>.requests` and `subscribe` on `<service>.replies.<self_origin_hash>`. The latter is naturally scoped to the caller's own origin (no other token has the right to subscribe to *your* reply channel).
2. **Service-level allowlist (new).** Add `rpc_services: Vec<String>` to `PermissionToken`. Server-side, the RPC fold rejects requests whose token doesn't list the service in scope; rejection is `RpcStatus::Unauthorized`. Empty list = no services allowed (defense-in-depth default; tokens predating the field don't authorize RPC).

End-to-end identity: `meta.origin_hash` is set by the bus from the AEAD-verified peer; not self-claimable. nRPC inherits the existing in-channel-identity-spoofability tradeoff (see `adapter/net/identity/origin.rs`).

## What naturally falls out of CortEX (free wins)

- **Crash recovery.** A request that was appended to the channel before the server crashed is replayed when the server's fold rehydrates from the log. The `applied_through_seq` strict-prefix watermark guarantees at-least-once handler execution. Pair with `IDEMPOTENT` flag for safe retry semantics.
- **Snapshot-based migration.** A server's in-flight RPC state migrates with the rest of its fold state (compute layer's snapshot/restore). In-flight calls survive a planned migration; in-flight calls survive a process restart.
- **Time-travel debugging.** "Which request caused the bad state?" — open the channel, replay events, see exactly which REQUEST flipped the fold into the broken state. Causal-chain integration shows the trace.
- **Audit trail.** Every RPC call is durable. Operators get a free per-service audit log without instrumenting handlers.
- **Backpressure.** RedEX append rate-limits naturally; over-cap publishers see `RedexError::Append` and surface it as `RpcError::Backpressure`.

## What we lose vs. a transport-level RPC (and why we're OK with it)

- **Per-call latency floor.** Each call goes through the redex append → fold dispatch → response publish → caller fold pipeline. Even with in-memory redex this is a few extra microseconds vs. a direct stream send/receive. Acceptable for any call where the network alone is hundreds of microseconds, which is every realistic microservice RPC.
- **Stream-level backpressure.** Stream-based RPC (gRPC's per-stream window) gives finer-grained flow control than channel-level append backpressure. For the streaming-response case (Phase 3) we may need to add channel-level credit grants — an extension of the existing `SUBPROTOCOL_STREAM_WINDOW` shape — but that's a small follow-up, not a blocker.
- **Direct unicast.** Every request goes through a channel even when the caller knows the target's `entity_id`. This is fine: the mesh's dispatcher already optimizes pub/sub to direct-deliver when there's a single subscriber. Queue-group dispatch is the same cost as broadcast-with-one-recipient.

## Phasing

| Phase | Release | Scope |
|------|---------|-------|
| **1** | v0.12 | `SubscriptionMode::QueueGroup(name)` lands on the channel layer; existing `Broadcast` semantics unchanged. `RpcServerFold` + `RpcClientFold` + the four `dispatch` constants. `Mesh::call` / `Mesh::serve_rpc` API. Channel naming convention enforced by helpers. Token-scope check (`rpc_services` allowlist on `PermissionToken`). Test suite covering: queue-group one-of-N delivery, correlation, deadline → CANCEL emission, idempotency replay, server panic, backpressure, token-scope rejection, identity guard. |
| **2** | v0.13 | `ServiceRegistry` derived from existing channel-subscription announcements. `Mesh::call_service` shortcut + routing-policy hooks (RoundRobin, P2C, Sticky, LowestLatency) wired into queue-group dispatch. Health-aware filtering against `proximity::node_health`. SDK typed wrappers for Rust / Node / Python / Go. |
| **3** | v0.14 | Streaming responses (`STREAMING_RESPONSE` flag → multiple `DISPATCH_RPC_RESPONSE` events with same `seq_or_ts` and a `is_terminal` payload bit). Per-streaming-response window grants. Caller-side helpers: `with_retry`, `with_circuit_breaker`, `with_hedge`. W3C Trace Context propagation hardened. Per-call latency / error-rate metrics on a Prometheus-compatible endpoint. |
| **deferred** | v0.15+ | Client-streaming, bidirectional streaming, schema registry / IDL codegen (`.nrpc` files → typed Rust/TS/Python clients). |

## Test surface

### Phase 1
- **Queue-group one-of-N delivery.** Spawn 4 servers in `QueueGroup("foo")`; publish 1000 requests; assert each request is processed by exactly one server, and load is approximately balanced (within 10% of even).
- **Queue-group + broadcast coexistence.** Same channel, mix of `Broadcast` subscribers (e.g., audit logger) and `QueueGroup("worker")` subscribers; assert broadcast subscribers see every event, queue-group sees one-of-N.
- **Correlation across concurrent calls.** Spawn N concurrent `call()` futures; assert each gets its own response keyed on the right `call_id`.
- **Deadline → CANCEL.** Caller's deadline fires before response; assert a CANCEL event is published; server's fold removes the in-flight entry; the response (if it was already mid-flight) lands on a non-existent oneshot and is dropped harmlessly.
- **Caller drop → CANCEL.** Caller drops the future; same CANCEL flow.
- **Idempotency replay.** Replay a request with `IDEMPOTENT` flag set after the original completed; assert the cached response is returned without re-running the handler.
- **Server panic.** Handler that panics surfaces as `RpcStatus::Internal` to the caller (caught by the spawn boundary's `JoinHandle` / `catch_unwind`).
- **Backpressure on overload.** Fill the redex append capacity; assert publishers see `RpcError::Backpressure`.
- **Token-scope rejection.** Token without `rpc_services` listing the service rejects the call with `Unauthorized`.
- **Identity guard.** `RpcContext::caller` is the AEAD-verified peer; not the value in the payload.
- **Crash recovery.** Append a request, kill the server before it processes, restart; assert the request is processed on rehydrate (at-least-once) and the response lands on the caller's reply channel.

### Phase 2
- **ServiceRegistry derivation.** Bring up N servers calling `serve_rpc("foo", ...)`; assert every node's local `ServiceRegistry` learns of "foo" within one capability-announcement interval.
- **Health-aware exclusion.** Mark one server unhealthy via `proximity`; assert subsequent calls don't route to it; assert recovery puts it back.
- **Routing-policy correctness.** `Sticky` is consistent across calls with the same key; `LowestLatency` picks the lowest-p50 instance.

### Phase 3
- **Streaming responses in order.** N RESPONSE events with same `seq_or_ts`; assert order; assert the terminal bit closes the call.
- **Stream cancellation mid-flight.** Caller cancels; assert subsequent RESPONSE events are dropped on arrival (the pending entry is gone).
- **Trace context propagation.** `traceparent` / `tracestate` round-trip through the headers block.

## Out of scope

- **Pub/sub replacement.** Channels-as-event-bus stay. RPC and events coexist on the same channel mechanism.
- **Service-mesh sidecar.** nRPC runs in-process.
- **Mutual TLS / cert rotation.** Net's existing AEAD + capability-token model is the substrate.
- **Schema-validated payloads in v1.** Payloads are `Bytes`; schema registry is deferred.
- **Sync RPC.** Async-only API.

## Open design questions

1. **Queue-group dispatch policy default.** P2C (against in-flight count, observed locally per channel publisher) is the recommended default; round-robin is the documented alternative. Either is fine for v1; pick one to ship and add the other later. Recommend P2C — it composes better with heterogeneous server capacity.

2. **Reply-channel naming with `origin_hash` vs `entity_id`.** `origin_hash` (8 bytes after the widening) is structurally fine for channel naming but has a known birthday-collision floor; using `entity_id` (32 bytes) eliminates collisions but produces a longer channel name. Per-caller reply channels are private to the caller anyway (capability tokens scope `subscribe` to your own `origin_hash`-named channel), so collisions don't cause cross-caller leakage — they just mean two callers share a channel. Recommend `origin_hash` for terseness; revisit if `entity_id`-keyed channels become a uniform convention elsewhere.

3. **Where does the queue-group selection happen — sender or receiver side?** Either works:
   - **Sender-side:** publisher consults the local roster, picks one queue-group member, sends to that one. Lower fan-out cost; biased view of who's healthy.
   - **Receiver-side:** publisher broadcasts to all queue-group members; each receiver picks a deterministic "should I take this one?" decision (consistent hash on `seq_or_ts`). Higher fan-out but unbiased.
   
   Recommend sender-side — it's what the existing dispatcher already does for unicast and it composes with the proximity-driven load metrics. Receiver-side is the fallback for cases where the sender doesn't have a complete roster view (e.g., partition healing).

4. **Idempotency cache eviction.** `completed_idempotent` LRU needs a sized bound. Default 10K entries per server fold? Per-caller? Per-(caller, service)? Recommend a single per-fold LRU sized at 10K with TTL of 5 minutes — covers reasonable retry windows without unbounded growth. Operator-tunable.

5. **Streaming-response ordering across queue-group failover.** If the server handling a streaming response dies mid-stream, queue-group dispatch reroutes subsequent events to a peer that has no context. Need either (a) sticky session affinity (queue group with `Sticky(call_id)` policy ensures all events for a call_id go to the same server) or (b) explicit takeover via the snapshot-restore path. Phase 3 problem; flagging early so the design accommodates it.
