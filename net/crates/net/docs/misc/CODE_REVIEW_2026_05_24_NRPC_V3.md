# Code review — `nrpc-sdks` branch (2026-05-24)

Branch base: `master`.
Scope: 37 commits ahead of master delivering NRPC v3 — substrate-level
cancel-token primitive, bounded-mpsc observer dispatch + drop counter,
cancel/AbortSignal wiring across Node/Python/Go bindings, typed
streaming SDK surfaces, and cross-lang integration tests. ~9,000 LOC
added / 640 removed across 25 files.

This is a separate branch from `subnet-scaling`; the prior four review
docs (`CODE_REVIEW_2026_05_2{3,4}_SUBNET_SCALING_PASS_{1..4}.md`) do
not overlap.

Three review agents (reuse / quality / efficiency) were dispatched in
parallel. Findings below are organised by severity, then category.
File paths are relative to repo root; line numbers reflect the branch
tip and may drift.

---

## HIGH — concurrency / allocation cost on the hot path

### N1 — `register_notify(0)` allocates a fresh `Arc<Notify>` per call

`net/crates/net/src/adapter/net/cancel_registry.rs:178-183`.

Every nRPC call (unary, streaming, client-stream, duplex) reaches
`register_notify(cancel_token)` at `mesh_rpc.rs:2156, 2379, 2526,
2943`. When `cancel_token == 0` (the overwhelmingly common path —
most callers don't pass a token), the registry returns
`Arc::new(Notify::new())` — one heap allocation for the `Arc` + one
for the `Notify` inner. The `tokio::select!` arm then awaits a Notify
that can never fire.

Cost: per call, ~80 bytes + 2 allocations + 2 atomic refcount
operations across every binding. With high call rates, this hits the
allocator pretty hard.

Fix: cache a single process-static `Arc<Notify>` (e.g. via
`OnceLock<Arc<Notify>>`) and clone the Arc on the zero-token path.
The Notify is never armed, so sharing it across all no-cancel calls
is safe and reduces the cost to a single atomic refcount bump.

### N2 — `CancelRegistry::gc()` runs on every `register_notify` with a full-HashMap scan

`net/crates/net/src/adapter/net/cancel_registry.rs:155, 185,
228-239`.

Comment at line 41-42 claims "not on any hot path" — that's wrong
now. `register_notify` IS on the hot path (every call). The gc
sweep walks the entire `HashMap` via `retain` under the mutex on
every new call: O(N) per call → O(N²) total for a burst of N
starts. At N = 10k in-flight calls, that's 100M HashMap entry
comparisons under a contended mutex per second of burst.

Fix options (pick one):

- Rate-limit GC: track `last_gc: Instant`, skip if elapsed < 1s.
- Move GC to a periodic background task spawned on the mesh.
- Only run GC inside `cancel()` (already opportunistic there; cancel
  rate is low and bounded).

### N3 — Mutex held across `Notify::notify_one` + `Arc::clone` in `register_notify`

`net/crates/net/src/adapter/net/cancel_registry.rs:184-202`.

The mutex covers the GC scan, the `entry` API insert, the
`Arc::clone`, AND the `notify_one` call. `parking_lot::Mutex` is
fast, but combined with N2 the critical section is long enough to
matter under contention.

Fix: extract `(notify, was_precancelled)` inside the lock, drop the
guard, then conditionally call `notify_one()`. Combined with N2,
removes the worst contention.

---

## HIGH — duplication / shape concerns

### N4 — Three identical bounded-mpsc observer trampolines

`bindings/node/src/mesh_rpc.rs:589-642` (`NodeRpcObserver`),
`bindings/python/src/mesh_rpc.rs:1645-1696` (`PyRpcObserver`),
`bindings/go/rpc-ffi/src/lib.rs:3092-3181` (`GoRpcObserver`).

All three rewrite verbatim: bounded `tokio::sync::mpsc::channel(1024)`,
`try_send` → on `Err` increment a `static AtomicU64`, spawn a drain
worker that loops `while let Some(evt) = receiver.recv().await`. Only
the per-event conversion + dispatch inside the worker loop differs.
The drop-counter test (`observer_drops_overflow_events_and_counts_them`)
is also copy-pasted into Node tests at `:2197` and Python tests at
`:2212` against the identical channel shape.

The Python file even acknowledges the duplication:
> "Matches the napi binding's `OBSERVER_BUFFER_CAPACITY` (1024) for
> consistency."

Fix: add `ObserverChannel<E>` (and a single shared
`OBSERVER_DROPPED_TOTAL` + `OBSERVER_BUFFER_CAPACITY`) to the
substrate, e.g. `src/adapter/net/cortex/rpc_observer.rs`. Takes an
`FnMut(E)` worker closure and returns a struct implementing
`RpcObserver`. Each binding writes only its language-native
conversion closure (~10 lines) instead of ~55. Removes the three
independent constants and atomics that already drift in commentary.

### N5 — Observer-event conversion happens BEFORE `try_send`, paying allocations for dropped events

`bindings/node/src/mesh_rpc.rs:437-462` (`impl From<&InnerRpcCallEvent>
for RpcCallEventJs`) + Python mirror at
`bindings/python/src/mesh_rpc.rs:~1495`.

`on_call` (Node line 632-641) builds the `RpcCallEventJs` BEFORE the
`try_send`. The conversion does `evt.method.clone()` +
`status_kind.to_string()` + `direction.to_string()` + 2 `BigInt::from`
allocations. When the channel is full and the event will be dropped,
we still pay 4-5 allocations per dropped event on the substrate
dispatch thread.

Fix: send `InnerRpcCallEvent` (or `Arc<InnerRpcCallEvent>`) through
the mpsc; convert to `RpcCallEventJs` / `PyRpcCallEvent` in the drain
worker. Hot path becomes `try_send(Arc::clone(&evt))` — no string
allocations on the dispatch thread, dropped events cost ~zero.
Composes naturally with N4.

### N6 — Inline cancel-watcher spawn boilerplate duplicated across three streaming entries

`net/crates/net/src/adapter/net/mesh_rpc.rs` at
`call_client_stream` (~L2155-2163), `call_duplex` (~L2378-2384),
`call_streaming` (~L2525-2531).

Each has the same 8-line block:

```rust
let cancel_token = opts.cancel_token.unwrap_or(0);
let cancel_notify = self.cancel_registry.register_notify(cancel_token);
let cancel_keep_alive = spawn_stream_cancel_watcher(
    // ... five identical args
);
```

Fix: extract a helper
`fn arm_stream_cancel(&self, opts: &CallOptions,
pending: &Arc<RpcClientPending>, call_id: u64) ->
StreamCancelKeepAlive` on `MeshNode` that wraps the three calls into
one. Drops ~24 lines and one easy-to-forget step.

### N7 — Unary `select!` arms each have identical 14-line cancel branch

`net/crates/net/src/adapter/net/mesh_rpc.rs:2941-2992`.

The two `tokio::select!` arms (no-deadline vs. with-deadline) each
have an identical 14-line cancel branch: release + record +
`fire_rpc_observer_outbound(..., Canceled, ...)` + `return
Err(RpcError::Cancelled)`. Any change to the observer event shape
requires updating both arms or risks drift.

Fix: extract a `fn fire_unary_cancel_outcome(&self, started_total,
target_node_id, service, request_bytes_len) -> RpcError::Cancelled`
helper that owns the release/metrics/observer/return tuple. Reduces
both arms to one match call.

---

## MEDIUM — surface / hygiene

### N8 — Asymmetric cancel surface across the three bindings

Three different mental models for the same primitive:

- **Node** (`bindings/node/mesh_rpc.ts:1934-1972`) — `opts.signal:
  AbortSignal`, internally mints token via `wireAbortSignal`.
- **Python** (`bindings/python/python/net/mesh_rpc.py:404-409` +
  `src/mesh_rpc.rs:217-287`) — `opts={'cancel': Cancellable}` object
  with pre-cancel latch.
- **Go** (`bindings/go/net/mesh_rpc_typed.go:521`) —
  `context.Context` with a watcher goroutine on `ctx.Done`.

All three serve the same primitive, but operators reading nRPC docs
cross-language hit three different mental models. The asymmetry
isn't documented anywhere user-facing.

Fix: (a) add a short cross-binding "cancellation cookbook" section
to `NRPC_V3_OBSERVER_MPSC_AND_CANCELLATION.md` showing the three
patterns side-by-side; (b) expose `cancelToken: bigint` /
`cancel_token: int` as the lowest-common-denominator in Python
(`opts['cancel_token']` accepting a raw int) so power users can opt
out of the object wrapper.

### N9 — Cancel token is a raw `u64` at every public boundary

`MeshNode::reserve_cancel_token() -> u64`, `CallOptions::cancel_token:
Option<u64>`, FFI `net_rpc_reserve_cancel_token() -> u64`, Node
`bigint`, Python `int`.

A misuse (passing another mesh's token, or passing `0` as a "real"
token) silently does nothing because the registries are per-mesh and
`0` is the sentinel.

Fix: `pub struct CancelToken(NonZeroU64)` in the substrate, with
`From`/`Into` only at the FFI boundary. Keeps the wire shape `u64`
but prevents the common typo. The `NonZero` constraint expresses
the `0`-is-no-token sentinel at the type level.

### N10 — Fixed sleeps in `integration_mesh_cancel.rs`

`net/crates/net/tests/integration_mesh_cancel.rs:150, 213, 288, 332,
376, 416`.

Six `tokio::time::sleep(Duration::from_millis(50..100))` calls
waiting for "the call to publish" or "the watcher to register." CI
flake source.

Fix: poll for `pending.len()` (or equivalent condition) with a 1-second
budget instead of guessing 50-100ms. At minimum, document why the
specific values were chosen so future readers know what each sleep
is waiting on.

### N11 — Stringly-typed observer status across bindings

`bindings/node/src/mesh_rpc.rs:402-405` (with explanatory comment),
`bindings/python/python/net/mesh_rpc.py:255`,
`bindings/go/net/mesh_rpc_typed.go:JSON tag site`.

`statusKind` is encoded as `'ok'|'error'|'timeout'|'canceled'`
strings in every binding because napi-rs `#[napi(object)]` doesn't
support tagged unions. A typo in a consumer's `if event.statusKind ===
'canceled'` check silently never fires.

Fix: at minimum, expose `STATUS_OK` / `STATUS_CANCELED` /
`STATUS_TIMEOUT` / `STATUS_ERROR` constants in each binding so
consumers don't hard-code the strings. Best fix: a small codegen
step from a single source-of-truth enum.

---

## LOW

### N12 — Per-binding `*Js` / `Py*` / `RpcCallEventC` POD mirrors

`RpcCallEventJs` (`bindings/node/src/mesh_rpc.rs:406-451`),
`PyRpcCallEvent` (`bindings/python/src/mesh_rpc.rs:~1495`),
`RpcCallEventC` (`bindings/go/rpc-ffi/src/lib.rs:~2562 + ~3146`).

Each defines the same eight-field shape plus an
`impl From<&InnerRpcCallEvent>` converter and a parallel
four-variant `RpcCallStatus*` mirror. Unavoidable for FFI since
shapes must be language-native, but the field-by-field copy logic
is mechanical. Worth flagging for the next code-generator pass.

### N13 — Narration comments in `cancel_registry.rs` + `mesh_rpc.rs`

`cancel_registry.rs` carries ~23 multi-line comments describing
WHAT each line does (e.g. lines 154-167 walk through what
`pre_cancelled = true; if marked_at.is_none() ...` is doing). The
code IS the doc.

Fix: keep the module-level "Model" / "Race fixes" preambles (those
carry irreplaceable rationale, including the CR-13 reference
documenting why), but trim the inline narrators to ≤2 lines focused
on WHY.

---

## Verified clean (false-positive notes)

- **`CancelRegistry` does NOT duplicate prior token primitives.** No
  `identity/token.rs` exists; the napi/Go local registries are now
  removed. The substrate primitive (`pre_cancelled` flag + orphan-TTL
  GC + 120s window) is correctly centralized; binding sites in
  `mesh_rpc.rs` correctly delegate.
- **AbortSignal listener doesn't leak.** Uses `{ once: true }` +
  explicit `removeEventListener` in `detach`
  (`bindings/node/mesh_rpc.ts:1963, 1969`).
- **Observer drop counter is optimal.** `AtomicU64` with `Relaxed`
  ordering across all 3 bindings (`node:602`, `python:1653`,
  `go:3102`).
- **`default_retryable` Cancelled arm is narrowly scoped.**
  `sdk/src/mesh_rpc_resilience.rs:149-152` adds exactly one arm,
  returns `false`, includes a commit-traceable comment.
  `default_breaker_failure` delegates to `default_retryable`
  (L710-712), so the fix propagates cleanly to the breaker without
  a parallel change. No widening.
- **`next_token` is cheap.** Single relaxed `AtomicU64::fetch_add`
  (`cancel_registry.rs:73-75`), monotonic, optimal.
- **`cancel(token)` is O(1).** HashMap lookup + idempotent
  `notify_one`. Stale tokens are safe (test pin at line 327-341).
- **Three binding pass-through layers are NOT redundant** with the
  substrate primitive — they bridge per-language ergonomics
  (AbortSignal / Cancellable / context.Context) to the same u64
  token.
- **AbortSignal wiring is single-helper** (`wireAbortSignal` at
  `bindings/node/mesh_rpc.ts:1934`), called from all four call
  shapes.
- **Golden vectors not duplicated** — only
  `tests/cross_lang_nrpc/golden_vectors_streaming.json` exists; no
  per-binding fixture mirrors.

---

## Suggested fix order

1. **N1, N2, N3** — hot-path perf wins. All local to
   `cancel_registry.rs`; tightly scoped. Together remove the worst
   allocator pressure + the quadratic burst behavior.
2. **N5** — observer-event Arc-clone instead of pre-converting.
   Composes naturally with N4 but valuable on its own.
3. **N4** — substrate `ObserverChannel<E>` consolidation.
4. **N6, N7** — substrate cancel-arm helpers.
5. **N10** — integration test sleep cleanup.
6. **N8, N11** — surface symmetry + magic-string constants.
7. **N9, N12, N13** — newtype token, code-generator follow-up,
   comment trimming.

N1+N2+N3 are the priority items: every nRPC call in production today
pays the unnecessary allocation, and the quadratic gc behavior is a
latent scalability bug that won't show up until a burst.
