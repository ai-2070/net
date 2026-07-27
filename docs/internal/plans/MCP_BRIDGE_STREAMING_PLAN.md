# Implementation Plan: MCP Bridge Streaming — events across the bridge, in both directions

**Goal:** lift the MCP bridge from "request/response only" to **streaming-capable in both directions**, honestly scoped to what MCP can actually carry: a wrapped MCP server's mid-call progress and log notifications become a real nRPC event stream on the mesh, and a native (or bridged) streaming capability's interim events reach an MCP host as progress notifications — with the terminal result unchanged in both cases. Cancellation propagates end to end. Everything is wire-additive; every v0.31 peer keeps working unmodified.

**Status (2026-07-10):** proposed. Follows the v0.31 "Hold The Line" bridge (`net-mesh-mcp`, `MCP_BRIDGE_PLAN.md`, `MCP_BRIDGE_SDK_PLAN.md`). v0.31 pinned `compat_tier: "mcp_bridge"` as request/response only and listed streaming as structurally deferred; this plan is that deferral, cashed.

**Position in the stack:** the SDK already ships the full streaming matrix — this plan adds **zero new wire dispatch IDs and zero new SDK streaming primitives**. It is adapter work: the bridge starts *using* `serve_rpc_streaming` / `call_streaming` and the tool-layer `ToolEvent` envelope that `serve_tool_streaming` / `call_tool_streaming` already speak. Same organizing observation as v0.31: the hard parts already existed; the work is an adapter over them.

---

## What "streaming both ways" honestly means

MCP `tools/call` is a single request with a single result. That does not change, and this plan does not pretend otherwise. What MCP *does* give us, on stdio, mid-call:

| MCP affordance (spec 2026-07-28, pinned) | Direction | What the bridge can do with it |
|---|---|---|
| `notifications/progress` (correlated by `progressToken` from the request's `_meta`) | server → client | interim progress events during a call |
| `notifications/message` (logging) | server → client | interim log events during a call |
| `notifications/cancelled` | client → server | abort an in-flight call |
| single `tools/call` result | server → client | the terminal event, unchanged |
| ~~client-streaming request~~ | — | **does not exist in MCP**; a bridged tool's *request* stays one message, permanently |
| partial results / Tasks extension | server → client | **not in the pinned base spec**; if the final spec or an extension adds it, it maps onto the same envelope via `spec/` (deferred, see Non-goals) |

So the two directions are:

- **Supply (`net wrap`)** — an MCP server that emits progress/log notifications during a call today gets them **silently dropped** (`wrap/stdio.rs:371`). After this plan, they ride onto the mesh as a server-streaming nRPC call: zero or more interim `ToolEvent`s, then exactly one terminal. A native mesh caller (SDK, Hermes, another shim) consumes a real stream with backpressure and cancel.
- **Demand (`net mcp serve`)** — a mesh capability that streams (native `serve_tool_streaming` tools, or bridged tools from a new wrap) is invocable from an MCP host with **live progress**: interim events forward to the host as `notifications/progress` when the host asked for them (by sending a `progressToken`), payload deltas aggregate into the final result, and the terminal event becomes the single `tools/call` result the host already expects.

Composed, the hero flow: Claude Code on machine A calls a pinned tool with a `progressToken` → shim opens a streaming invoke across the mesh → wrap on machine B injects a `progressToken` into the child's `tools/call` → the child's progress notifications flow child → wrap → mesh → shim → host, live — and a cancel from the host walks the same path back.

Server-streaming is the only new shape the bridge speaks. Client-streaming and duplex over a bridged tool remain structurally impossible (one MCP request per call) and stay out of the compat tier — stated on every page, as today.

---

## Current state (verified in-tree)

Supply side:
- Each wrapped tool is served with the raw **unary** `Mesh::serve_rpc(&tool_id, handler)` — `wrap/session.rs:551` (`publish_server`), `:699` (`publish_tools`). The handler is hand-rolled (`WrapInvokeHandler`, `wrap/invoke.rs:311-463`) because admission needs the `RpcContext`: delegation/owner-scope → arg parse → payment → policy → invoke → encode. One `RpcResponsePayload` out, ever.
- `StdioMcpClient::call_tool` issues `tools/call` and awaits the reply by JSON-RPC id (`wrap/stdio.rs:150-161`, `:193-226`). The stdout reader's `dispatch_line` handles `tools/list_changed` only; **every other notification — including `notifications/progress` and `notifications/message` — is explicitly ignored** (`wrap/stdio.rs:368-371`, comment: "not part of the compat tier").
- Descriptors force `streaming: false` (`wrap/descriptor.rs:352`); `COMPAT_TIER_MCP_BRIDGE = "mcp_bridge"` (`wrap/descriptor.rs:29`) is documented "request/response only".

Demand side:
- The gateway invokes with **unary** `Mesh::call` (`serve/mesh_gateway.rs:176`, via `call_once` / `call_retry` / `invoke_on`); the `CapabilityGateway::invoke` trait returns one `CallToolResult` (`serve/backend.rs:192-198`).
- The shim replies with a single JSON-RPC result (`serve/shim.rs:268-286`) and never emits progress or logging to the host. `progressToken` / `_meta` is not read anywhere. `notifications/cancelled` is a no-op (`serve/shim.rs:216-221`).
- Retry-on-timeout is gated by `InvokeSafety` (`serve/backend.rs:130-161`; applied at `serve/mesh_gateway.rs:609-613`): only `credential_status: none` unpaid tools re-run a timed-out call.

The SDK underneath (what the adapter is allowed to ride, and all of it public):
- Full streaming matrix on `Mesh`: `serve_rpc_streaming` / `_typed`, `call_streaming` / `_typed` / `call_service_streaming_typed`, plus client-stream and duplex (`sdk/src/mesh_rpc.rs:423-712`). Response-direction flow control via `nrpc-stream-window-initial` + `DISPATCH_RPC_STREAM_GRANT`; dropping the caller's stream emits CANCEL.
- The tool layer already defines the interim envelope: **`ToolEvent`** — `Start` / `Progress` / `Delta` / terminal `Result` / terminal `Error` — with a pinned lifecycle contract: zero or more non-terminal events, then exactly one terminal; a handler stream that ends without a terminal gets a synthesized `Error { code: "missing_terminal" }` (`sdk/src/tool.rs:482` `serve_tool_streaming`, `:620` `call_tool_streaming`).
- `ToolDescriptor.streaming: bool` exists in the vocabulary (`src/adapter/net/cortex/tool.rs:141`) and is announced via `tool::<id>::streaming` metadata.

The bridge is unary end-to-end **by its own choice** (the compat tier), not by any SDK or daemon limitation. That is precisely why this plan is small.

---

## Doctrine (carried forward, plus what streaming adds)

1. **Core purity, SDK only.** No new MCP awareness below the adapter. The adapter keeps riding `net-mesh-sdk` public surface only; the dependency-boundary CI test keeps enforcing it. If the bridge needs a streaming primitive the SDK lacks, the SDK grows a public primitive first.
2. **Wire-additive, four corners green.** Old wrap ↔ old shim, old wrap ↔ new shim, new wrap ↔ old shim, new wrap ↔ new shim: all four combinations must work. A v0.31 unary caller invoking a streaming-registered bridged tool gets exactly what it gets today — one terminal result. Streaming is something a *caller opts into*, never something a provider imposes.
3. **One invoke, one consent, one gate.** Streaming does not change the consent model: the fail-closed gate (pin store reload, credential-status distrust, owner-scope on the AEAD-verified origin) runs **once, before the stream opens**. There is no per-event consent and no way for a stream to outlive a revocation cheaper than cancellation (revoke → next invoke denied; in-flight streams are cancellable, not retroactively filtered).
4. **A stream is never retried.** A half-consumed stream cannot be replayed, and interim events may already have been observed. Streaming invokes are at-most-once for *every* tool — stricter than unary `InvokeSafety`, which keeps its existing duplicate-safe retry for unary calls only. A broken stream surfaces as a terminal error (`stream_interrupted`), never a silent re-run.
5. **Secrets never ride interim frames.** The v0.31 token-leak invariant extends to every new frame type: a credential env var must not appear in a progress message, a log notification, a delta, or a terminal event. The sentinel CI test grows stream coverage; progress/log bodies are treated as attacker-influenced strings (bounded, never interpolated into shim/wrap logs at default level).
6. **The model cannot approve its own access — still.** Progress events are *output*, not a side channel for widening authority. Nothing in an interim event can trigger a pin, a consent change, or a retry.
7. **Spec churn stays in `spec/`.** `progressToken` placement, notification shapes, and any future partial-result/Tasks mapping live behind the spec module (`spec/mod.rs`, `PROTOCOL_VERSION = "2026-07-28"`). If the final spec moves, the adapter churns; the envelope on the mesh does not.
8. **Honest advertisement.** A tool that can only ever emit a terminal event does not advertise streaming. `stream_support` says what a provider will actually do, and the demand side treats it as wire-declared (untrusted for consent purposes, like `credential_status`).

---

## Design

### The envelope: `ToolEvent`, not a new vocabulary

Interim frames on the mesh are the SDK's existing `ToolEvent` JSON envelope, one event per streaming chunk — the same shape `serve_tool_streaming` / `call_tool_streaming` already produce and consume, so a *native* SDK caller can consume a bridged streaming tool with `call_tool_streaming` and cannot tell it is bridged (beyond the descriptor metadata). Mapping:

| Source | `ToolEvent` |
|---|---|
| wrap: MCP `notifications/progress { progress, total?, message? }` | `Progress { progress, total, message }` |
| wrap: MCP `notifications/message` (logging) | `Delta` carrying a structured log payload (level, logger, data), tagged as log-typed |
| wrap: `tools/call` result | terminal `Result` (same `CallToolResult` lowering as today) |
| wrap: `tools/call` error / child exit mid-call | terminal `Error` (same failure lowering as today; `stream_interrupted` when the child dies mid-call) |
| shim ← native tool `Start` / `Progress` | host `notifications/progress` (when the host sent a `progressToken`) |
| shim ← native tool `Delta` | aggregated into the final result (v1 default); optionally surfaced as progress `message` when small — see demand side |
| shim ← terminal `Result` / `Error` | the single `tools/call` JSON-RPC result / error, exactly as today |

The exact `ToolEvent` JSON shape gets golden vectors (it becomes cross-boundary wire the moment the bridge speaks it between independently-versioned nodes), joining the `tests/cross_lang_mcp/` vector suite.

### Descriptor and tier: advertise, don't fork the tier

- `compat_tier` stays `"mcp_bridge"`. Streaming does not graduate a bridged tool out of the tier — artifacts and migration are still structurally out, and forking the tier string would needlessly split every existing consumer's expectations.
- New bridge metadata key `tool::<id>::stream_support` with values `none | progress | events` (wire-additive; v0.31 peers drop unknown keys, and absence means `none`):
  - `none` — unary only (default; exactly today's behavior),
  - `progress` — interim `Progress`/log events possible, payload arrives only in the terminal event (the honest ceiling for a base-spec MCP child),
  - `events` — full `ToolEvent` interim stream (native tools re-exposed through the shim; bridged tools if a future spec adds partial results).
- `ToolDescriptor.streaming` flips `true` iff the wrap side actually registered a streaming handler for that tool.
- **Grouping/failover:** the equivalence fingerprint folds `stream_support` alongside schema/credential status, so a streaming and a non-streaming provider of the "same" tool never collapse — mid-stream failover is meaningless (doctrine 4: a stream is never retried, so it can never be failed over either).
- The pin store is untouched (it stores `cap_id` + state only; schemas and `stream_support` are fetched live at `tools/list` / invoke time, as today — `serve/shim.rs:327-340`).

### Supply side (`net wrap`)

**Registration.** `publish_server` registers each tool's service with the raw streaming handler (`Mesh::serve_rpc_streaming`) instead of unary `serve_rpc`, keeping the hand-rolled admission pipeline intact — the streaming context carries the same AEAD-verified `caller_origin`, and delegation → args → payment → policy all run *before* the first frame is emitted, exactly where the unary handler runs them today (`wrap/invoke.rs:313-461` order preserved). Only after admission does the handler start emitting: optional `Start`, interim events as they arrive from the child, one terminal.

**Unary fallback is non-negotiable (doctrine 2).** A v0.31 shim or SDK caller does unary `Mesh::call` against the same service name. Phase 0 pins how: either the substrate's streaming fold already collapses to a single terminal RESPONSE when the caller didn't set `FLAG_RPC_STREAMING_RESPONSE` (then nothing to do), or the bridge/SDK adds that collapse (buffer nothing, emit only the terminal as the one RESPONSE), or the tool dual-registers. Whatever the mechanism, the observable contract is fixed: **unary caller → single terminal result, byte-compatible with today**.

**Progress plumbing in `StdioMcpClient`.**
- Per in-flight call, inject `_meta.progressToken` into the outgoing `tools/call` (token derived from the JSON-RPC request id — collision-free by construction). Injection lives in `spec/`.
- The stdout dispatcher grows a correlation map `progressToken → bounded per-call event sender` (the `dispatch_line` catch-all at `wrap/stdio.rs:371` stops dropping `notifications/progress` and `notifications/message`; unknown tokens are still dropped — a notification for a call we don't know is noise, not a wedge).
- **Bounds everywhere:** per-call interim queue is bounded (default 64 events). On overflow, progress events **coalesce** (keep latest progress; count dropped log events and note the count on the next delivered event) rather than blocking the stdout reader — the v0.31 flood-hardening stance (bounded 32 MiB line reader, reply reader off the drain path) extends to notifications; a flooding child can never wedge the pipe or exhaust memory.
- Mesh-side backpressure: the wrap's streaming emitter opts into the response window (`nrpc-stream-window-initial`, default 64) so a slow/stalled mesh consumer stalls the *pump*, and the pump's stall coalesces at the bounded queue rather than back into the child's stdout.

**Cancellation.** The nRPC substrate delivers CANCEL when the caller drops its stream. The wrap handler's cancel token now does real work: send MCP `notifications/cancelled { requestId }` to the child, stop the correlation entry, emit nothing further. A child that keeps streaming after cancel is ignored (its token is unregistered). The child's *result* arriving after cancel is dropped — the call is already terminally `Cancelled` on the mesh side. At-most-once holds: cancel never triggers a re-run.

**Payment.** Paid bridged tools may stream: the payment gate runs at admission (before the first frame), settlement/billing semantics are unchanged — one invoke, one billing event, emitted on terminal exactly as the unary path does today. The SDK's `serve_tool_streaming` refuses `pricing_terms` because *it* has no gate; the bridge's raw hand-rolled path is the gate, so this restriction does not bind here. One pinned rule: **no partial-delivery refunds** — a paid stream that the caller cancels mid-way is a served (billed) invoke; docs say so.

**Lifecycle.** `tools/list_changed` reconciliation (`wrap/session.rs` refresh) re-derives `stream_support` per tool on every refresh; a server that stops emitting progress after an update simply re-announces `none`, and in-flight streams are unaffected (their correlation entries survive until terminal/cancel).

### Demand side (`net mcp serve`)

**Gateway.** `CapabilityGateway` grows `invoke_streaming(id, args, safety) → event stream` alongside unary `invoke` (`serve/backend.rs:192-198`); `MeshGateway` implements it over `Mesh::call_streaming` against the same resolved node/service that `call_once` uses today (`serve/mesh_gateway.rs:176`), decoding each chunk as `ToolEvent`. Selection rule: the gateway opens a streaming invoke **only when** the descriptor advertises it (`streaming: true` on native tools / `stream_support != none` on bridged metadata) *and* the caller asked for events; everything else stays on the existing unary path with its existing retry semantics. Streaming invokes take the no-retry branch unconditionally (doctrine 4) and replace the flat whole-call deadline with **idle-timeout-between-events** (default = the existing invoke timeout) plus an overall hard cap.

**The host opt-in signal is the `progressToken`.** No new meta-tool, no new argument schema: an MCP host that wants progress sends `_meta.progressToken` on `tools/call` — that is precisely what the field is for. `handle_tools_call` (`serve/shim.rs:268-286`) reads it; if present *and* the capability advertises streaming, the shim invokes via `invoke_streaming` and forwards:
- `Start` / `Progress` → `notifications/progress { progressToken, progress, total?, message? }` written to the host, **bounded and coalesced** (latest-progress-wins; never block on host stdout — the shim's write side gets the same off-the-read-path treatment the wrap's did in v0.31),
- log-typed `Delta` → `notifications/message` (host permitting; behind a shim flag, default on),
- payload `Delta` → accumulated; concatenated/merged into the terminal result (v1 default — base MCP has nowhere else to put payload), never emitted as a notification,
- terminal `Result` / `Error` → the single JSON-RPC result/error, exactly as today.
- No `progressToken`, or a `none`-support capability → today's unary path, byte-identical behavior.

This applies to **both** promoted (pinned) tools and `net_invoke_capability` — the token rides the same way on either surface. `net_describe_capability` output grows the `stream_support` field so the model can see which tools are worth a token.

**Cancellation.** `notifications/cancelled` from the host (`serve/shim.rs:216-221`, today a no-op) now maps `requestId → in-flight invoke` and cancels it: drop the gateway stream → substrate CANCEL → provider → (if bridged) child `notifications/cancelled`. The shim then emits nothing further for that request id (per MCP, a cancelled request gets no response). Unary in-flight invokes become cancellable too — same map, the win is free.

**Consent, unchanged by construction.** The gate (`serve/gated.rs`) wraps `invoke_streaming` exactly as it wraps `invoke`: decision before open, per-invoke pin-store reload, wire-declared `stream_support` never consulted for consent. A `requires_approval` capability is exactly as blocked from streaming as from unary invoke.

**Interim events are untrusted input.** Progress messages and log bodies originate from a remote provider (possibly another root identity, if widened). The shim caps per-event message length, caps events-per-call forwarded to the host, strips control characters, and never logs bodies at default verbosity. The security review (Phase 3) treats "provider injects instructions into progress messages shown to the model" as a first-class scenario — mitigation is bounding + provenance (notifications carry the capability id), not content inspection.

### Four-corner compatibility matrix (pinned as tests)

| Caller ↓ / Provider → | v0.31 wrap (unary) | new wrap (streaming-registered) |
|---|---|---|
| v0.31 shim / SDK unary `call` | today, untouched | **single terminal result, byte-compatible** (Phase 0 mechanism) |
| new shim, no `progressToken` | unary path, as today | unary path, as today |
| new shim + `progressToken` | unary path (descriptor says `none` → no streaming attempt) | live progress + terminal result |
| native SDK `call_tool_streaming` | `NoRoute`/unary as today | full `ToolEvent` stream |

---

## Phases

Each phase is independently mergeable, keeps the whole existing test suite green, and lands with its own regression tests.

### Phase 0 — Substrate verification spike + pinned decisions

Small, throwaway-code phase; its output is *facts and pinned decisions*, recorded in this file.

- [ ] **Verify the unary-caller-on-streaming-service behavior** in `cortex::rpc`: does `RpcServerStreamingFold` collapse to a single terminal RESPONSE when the incoming REQUEST lacks `FLAG_RPC_STREAMING_RESPONSE`? Write the failing/passing test first (`a_unary_caller_against_a_streaming_service_gets_one_terminal_response`).
  - If yes: pin it with the test; done.
  - If no: decide between (a) substrate fix — streaming fold honors the absent flag by emitting only the terminal frame as a plain RESPONSE (additive, no wire change, ships with its own tests in the core), or (b) bridge-side dual registration. Bias: (a) — it fixes the seam for every SDK user, not just the bridge, and the flag is already the caller's declared contract.
- [ ] Verify CANCEL delivery to a raw `RpcStreamingHandler`'s context token when the caller drops mid-stream (exists per the bidi plan; pin with a bridge-shaped test).
- [ ] Pin the `ToolEvent` JSON encoding with golden vectors (`tests/cross_lang_mcp/tool_event_vectors.json`), including the log-typed `Delta` shape this plan adds a convention for.
- [ ] Confirm `_meta.progressToken` request shape + progress/logging notification shapes against the pinned 2026-07-28 spec text; land the DTOs in `spec/` (nothing outside `spec/` parses raw notification JSON).
- [ ] Decide the default bounds (per-call interim queue 64, stream window 64, per-event message cap, events-per-call cap) and record them here.

**Acceptance:** the four facts above are pinned by tests or recorded decisions; no bridge behavior has changed.

### Phase 1 — Supply side: `net wrap` streams

- [ ] `spec/`: `progressToken` injection into outgoing `tools/call`; typed `ProgressNotification` / `LoggingNotification` parsing.
- [ ] `wrap/stdio.rs`: correlation map (token → bounded sender); `dispatch_line` routes progress/logging to it; unknown-token and post-cancel notifications dropped and counted; flood-coalescing per the bounds from Phase 0.
- [ ] `wrap/invoke.rs`: `WrapStreamingInvokeHandler` — same admission pipeline, then pump: interim `ToolEvent`s from the correlation entry, terminal from the `tools/call` reply; child-death mid-call → terminal `Error { code: "stream_interrupted" }`; cancel token → `notifications/cancelled` to the child + entry teardown.
- [ ] `wrap/session.rs`: register via `serve_rpc_streaming` (or the Phase 0 mechanism); flow-control window on; unary fallback proven.
- [ ] `wrap/descriptor.rs`: `stream_support` metadata key + `descriptor.streaming` set from the registration path; `schema_hash` / equivalence fingerprint folds `stream_support`.
- [ ] Fixture (`net-mcp-fixture`): new behaviors — `progress` (N progress notifications then result), `chatty` (log notifications), `flood` (progress storm, exercises coalescing), `cancel_aware` (records receipt of `notifications/cancelled`, exits the call), `slow_progress` (progress with configurable gaps, exercises idle timeout later). All hermetic, on command.
- [ ] **Token-leak test extended:** `a_credential_env_never_appears_in_a_stream_frame` — sentinel threaded through a streaming invoke; assert absent from every interim frame, wire capture, and log.
- [ ] Tests: streaming round-trip over real network (N progress + terminal, in order); unary caller gets terminal only; cancel from a native caller reaches the fixture (`cancel_aware` proves it); flood coalesces without wedging `tools/list_changed` handling on the same pipe; paid streaming tool bills exactly once on terminal; `refresh()` re-derives `stream_support`.

**Acceptance:** fixture's `progress` tool wrapped on one node, consumed from another via `call_tool_streaming` with live interim events; every v0.31 demand-side test passes unmodified against the new wrap.

### Phase 2 — Demand side: `net mcp serve` forwards progress

- [ ] `serve/backend.rs`: `invoke_streaming` on `CapabilityGateway` (+ DTOs: interim event, terminal); `InvokeSafety` docs note streaming is unconditionally at-most-once.
- [ ] `serve/mesh_gateway.rs`: implement over `Mesh::call_streaming`; no-retry branch; idle-timeout-between-events + hard cap; `stream_interrupted` terminal on transport loss.
- [ ] `serve/gated.rs`: gate wraps `invoke_streaming`; consent decision precedes stream open; per-invoke pin reload as today.
- [ ] `serve/shim.rs`: read `_meta.progressToken`; in-flight request map (id → cancel handle); `notifications/cancelled` cancels (unary too) and suppresses the response; bounded, coalescing notification writer off the read path; `notifications/progress` / `notifications/message` emission; payload-`Delta` aggregation into the terminal result; per-event and per-call caps; `net_describe_capability` surfaces `stream_support`.
- [ ] Tests: host-with-token sees ≥1 progress then one result (fixture `progress` behind a wrap, full two-node loop); host-without-token byte-identical to v0.31; cancelled mid-stream → child sees `notifications/cancelled`, host gets no response for that id; flood → coalesced, host pipe never blocks; consent-gated capability denied before any frame; idle timeout surfaces `stream_interrupted`; new shim against v0.31 wrap identical to today (corner 3).

**Acceptance:** the composed hero flow works over a two-node loopback mesh: MCP host → shim → mesh → wrap → fixture child, progress live end to end, cancel walking the reverse path; the four-corner matrix is green as named tests.

### Phase 3 — Hardening, security review, host matrix

- [ ] **Security review (before release), scenarios at minimum:** progress-message prompt injection from a widened-scope provider; notification flood DoS on either side; secrets in interim frames (the extended sentinel test); cancel-race double-execution (prove a cancel can never cause a re-run); a hostile provider declaring `stream_support` it doesn't honor (must degrade to terminal-only, never hang — the idle timeout is the backstop); revocation vs in-flight stream (documented: cancel, not retroactive filtering).
- [ ] Host matrix extended with columns: `progressToken` sent?, progress rendering, `notifications/message` handling, cancel emission — populated for Claude Code, Cursor, and one more host.
- [ ] Metrics: time-to-first-event, interim events per call, coalesce-drop counts (both sides), cancel-propagation latency, `stream_interrupted` rate.
- [ ] Chaos-shaped tests: child killed mid-stream; mesh partition mid-stream; host killed mid-stream (shim cleans up the invoke); shim killed mid-stream (wrap sees CANCEL via transport teardown or times out its pump).

**Acceptance:** review findings fixed with regression tests, suite/clippy/doc green; host matrix recorded in-tree.

### Phase 4 — Bindings + golden vectors

Per the SDK matrix rules (surfaces ship with named consumers; bindings marshal, never implement):

- [ ] `ToolEvent` + `stream_support` golden vectors verified from Python / Node / Go (extends `tests/cross_lang_mcp/`).
- [ ] `lower_tool` / `classify` helper parity updated for the new metadata key — byte-identical DTOs across bindings, as the existing helper vectors.
- [ ] Python/Node: the *native* streaming consume path is general-SDK work already tracked by the nRPC bindings parity plan; this plan's binding surface is only the bridge vocabulary above. Explicitly staged: no binding-side wrap/shim streaming internals (they stay Rust, per doctrine — the shim is a binary).

**Acceptance:** vector suites green in all bindings; no binding gained logic.

### Phase 5 — Docs + release integration

- [ ] Every page that pins "request/response only — no streaming" (the v0.31 claims audit touched each one) updates to the honest new line: *bridged tools stream progress and events; artifacts and migration remain native-only*. The tier table's "Native Net" row keeps artifacts/A2A/migration as the graduation story.
- [ ] Guides: both bridge directions re-walked with a progress-emitting example; `net_describe_capability` docs show `stream_support`; the "no partial-delivery refunds" rule lands on the payments pages that mention bridged paid tools.
- [ ] SDK spine `watch`/`invoke` pages note the bridge now honors the same `ToolEvent` contract.
- [ ] Pre-flight claims audit rerun on the changed pages (no claim ships uncashed).

---

## Non-goals (this plan)

- **Client-streaming or duplex over a bridged tool.** MCP has one request per call; structurally out, permanently, and documented as such.
- **Partial results / MCP Tasks extension.** Not in the pinned base spec. The envelope is ready for it (`Delta`, `stream_support: events`); the mapping lands in `spec/` if/when the final spec does. Evaluate after 2026-07-28 finalizes.
- **Resumable / replayable streams.** A broken stream is a terminal `stream_interrupted`; no resume tokens in v1.
- **Mid-stream failover.** Equivalence-collapse across providers stays fingerprint-split by `stream_support`, and a stream is never retried — failover remains a unary-only, opt-in feature.
- **Artifacts and migration over the bridge.** Still native Net capabilities; the tier story is unchanged.
- **Streaming through the HTTP/remote MCP path, OAuth, `--public`.** Stdio-only, as v0.31.
- **Sampling (`sampling/createMessage`) bridging.** Server-initiated requests remain method-not-found, as today.

## Risks

| Risk | Mitigation |
|---|---|
| Substrate doesn't collapse streaming→unary for old callers (Phase 0 finds the seam missing) | Preferred fix is the small additive substrate change; fallback is bridge-side dual registration. Either way corner 2 of the matrix is a named test before Phase 1 merges |
| Notification flood (hostile/buggy child or provider) wedges a pipe or the host | Bounded queues + coalescing on both sides, writers off the read path (v0.31 pattern), events-per-call caps; `flood` fixture behavior in CI |
| Progress messages become a prompt-injection channel into the model | Length caps, control-char stripping, provenance (capability id on every notification), security-review scenario with regression test |
| Secrets leak via progress/log bodies | Sentinel token-leak test extended to every frame type; log redaction at default verbosity |
| Cancel races re-execute a side effect | Cancel never re-runs (doctrine 4); at-most-once pinned by the `cancel_aware` fixture test |
| Host support for `progressToken`/notifications is spotty | Opt-in by token — a host that never sends one sees zero behavior change; host matrix tracks reality |
| Paid streams cancelled mid-way dispute billing | Pinned rule: admission-gated, billed once on terminal, no partial refunds — on the payments docs pages |
| Spec final (2026-07-28) shifts notification shapes | All parsing/injection in `spec/`; one owner watches the changelog through final (existing workstream) |
| Scope creep toward "MCP is now streaming-native" | `stream_support` ceilings are honest (`progress` for base-spec children); tier language unchanged; docs audit re-run |

## Open questions

1. **Phase 0's fork:** substrate collapse vs dual registration — decided by the spike, recorded here.
2. Should log-typed `Delta` forwarding to hosts (`notifications/message`) be on or off by default? Lean **on**, flag to disable, since hosts already gate logging display.
3. Do we surface a `stream: false` per-call opt-out on `net_invoke_capability` for models that sent a token but want quiet calls? Lean no — the token *is* the opt-in; removing it is the opt-out.
4. Default idle timeout: reuse the invoke timeout or a separate knob? Lean reuse, one knob fewer, revisit if `slow_progress` testing says otherwise.
