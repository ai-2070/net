# Code review — `nrpc-streaming` branch (2026-05-19)

Branch base: `master` at `1fde3570eb68f12ac08898355e31796423474cd3`.
Branch tip: `986f151f` ("Tidy `Some(0)` regression tests for workspace-wide clippy").
Scope: ~11.6k LOC across the Rust substrate, Rust SDK veneer, C ABI, Go/Node/Python bindings, benches, integration tests, cross-lang conformance fixture, and plan docs. Implements bidirectional streaming for nRPC (client-streaming + duplex) per `docs/plans/NRPC_BIDI_STREAMING_PLAN.md`.

Findings below are organised by severity. File paths are relative to repo root unless stated otherwise. Line numbers reflect the branch tip and may drift with subsequent commits.

---

## HIGH

### H1 — Drop-ordering race causes silent response loss on clean drop
`net/crates/net/src/adapter/net/mesh_rpc.rs:824` (`ClientStreamCallRaw::drop`) and `:874` (`DuplexInner::drop`). `pending.cancel(call_id)` removes the pending slot synchronously, but the CANCEL publish is `tokio::spawn`'d. If a terminal RESPONSE lands on the reply channel between the synchronous removal and the spawned CANCEL ship, the response is silently dropped. For duplex this also discards in-flight non-terminal response chunks. Order should be: spawn the CANCEL publish first, then remove the pending entry (the entry already filters responses arriving after the server's terminal frame, so leaving it briefly is safe).

### H2 — Silent drop of the first request body on a fresh mpsc
`net/crates/net/src/adapter/net/cortex/rpc.rs:2585`, `:2625`, `:1261`. `tx.try_send(...)` on a freshly created `mpsc::channel(STREAMING_REQUEST_PUMP_CAPACITY)` should never fail, but the `.is_err()` branch only logs and continues — the handler then spawns with a stream that's missing its first item but appears valid. Make it `expect`, or convert to a synthetic Internal error and fail the call.

### H3 — Request-direction GRANT spawn-storm under bursting
`cortex/rpc.rs:447-449`. `RequestStream::poll_next` emits exactly one `REQUEST_GRANT` per `Ready(Some)` via `tokio::spawn` in the emitter, regardless of declared window. A 1024-chunk burst at the handler produces 1024 individual GRANT publishes (each with its own `tokio::spawn` + AEAD allocation). The response direction batches grants via `>>4`; the request direction must do the same.

### H4 — Orphan handler state if `FLAG_REQUEST_END` publish errors
`mesh_rpc.rs:687-727` (`ClientStreamCallRaw::finish`) and the duplex twin in `DuplexSink::finish_sending`. If the terminal-END publish fails with a transport error, the function returns `Err` and `Drop` fires CANCEL — but if the CANCEL publish also fails, the server never sees END or CANCEL. The per-call mpsc + handler task in `RpcStreamingRequestFold::senders` is orphaned with no server-side reaper. `STREAMING_REQUEST_PUMP_CAPACITY=1024` bounds memory per orphan but the orphan count itself is unbounded.

### H5 — Go FFI duplex breaks under concurrent send+recv
`net/crates/net/bindings/go/rpc-ffi/src/lib.rs`, every `net_rpc_duplex_*` shim (`_send`, `_finish_sending`, `_next`, `_sink_send`, `_stream_next`, `_request_stream_next`). The pattern `inner.lock().take() ... block_on ... *inner.lock() = Some(call)` is not safe under concurrent use: while one cgo call is blocked in `block_on`, a second concurrent call sees `None` from the take, latches `done=true`, and returns `STREAM_DONE`. From Go, a user racing two `Send`s — or `Send` racing `Recv` on `DuplexCall`, which is the primary duplex use case — spuriously CANCELs the call.

### H6 — Zero-length response Box leak on every streaming chunk (Go)
`net/crates/net/bindings/go/net/mesh_rpc.go:1387-1395` and twins in `DuplexCall.Recv`, `DuplexStream.Recv`, `RequestStreamRecv.Recv`. On `code == 0 && outBodyLen == 0` the path returns `[]byte{}, nil` without calling `net_rpc_response_free`. Rust's `write_response` always heap-allocates a `Box<[u8]>` (even zero-length) and `net_rpc_response_free` short-circuits on `len == 0` — the Box leaks. The pre-existing unary path has the same shape, but every new streaming code path inherits the leak; on high-throughput streaming RPCs it is a fast leak.

### H7 — Use-after-free if Go captures stream/sink past the trampoline
`mesh_rpc.go:1681-1701` (`RequestStreamRecv.Recv`) and `ResponseSinkSend.Send`. Docstring warns "MUST NOT be retained past the callback", but nothing enforces it. If user handler code spawns a goroutine capturing `stream`/`sink` and returns, Rust frees the underlying `RpcRequestStreamHandleC` / `RpcResponseSinkHandleC` via `Box::from_raw` in the `spawn_blocking` closure. A subsequent `Recv()` / `Send()` from the leaked goroutine calls FFI on a freed pointer → UAF. The `r == nil || r.handle == nil` check does not help (handle still has its old non-nil value). Add a Go-side `closed atomic.Bool` flipped before the trampoline returns.

### H8 — Go binding Split/Close/Finish double-free races
`mesh_rpc.go:1573-1602` (`DuplexCall.Split`) calls `closed.Store(true)` instead of `Swap`; a concurrent `Close()` getting `Swap(true) == false` proceeds to call `_duplex_free` a second time → double-free. Same shape in `ClientStreamCall.Finish` at line 1377: `c.closed.Store(true)` after the cgo call lets a concurrent `Close()` between cgo-return and Store double-free via the deferred `_free`. Use `Swap(true)` with explicit early-return on already-closed.

### H9 — Node/Python response-sink backpressure is absent
`net/crates/net/bindings/node/src/mesh_rpc.rs:434-443` (`JsResponseSink::send`) and `net/crates/net/bindings/python/src/mesh_rpc.rs:507-516` (`PyResponseSinkSend::send`). Both go straight to `RpcResponseSink::send`, which is non-async with no credit await. JS/Python handlers that emit faster than the client drains buffer unboundedly in the SDK's internal sink, defeating the entire point of duplex flow control. Either expose an async `send` or document loudly that response-side flow control is not honoured at the binding layer.

### H10 — Node `serve_*` handler timeout only bounds JS dispatch, not the call body
`node/src/mesh_rpc.rs:532-565` (`serve_client_stream`) and `:610-643` (`serve_duplex`). `tokio::time::timeout(self.timeout, rx)` wraps only the TSFN enqueue → JS callback synchronous return of the Promise object. Once `let promise = ...` is acquired, `promise.await` has no timeout — a hung JS handler pins a Rust task and a TSFN ref for the full call lifetime. Python's `spawn_blocking + timeout` (`python/src/mesh_rpc.rs:537-570`) bounds the whole call, so behaviour diverges between bindings under the same field name.

### H11 — CI silently skips the cross-language streaming test
`.github/workflows/ci.yml:165-177`. `integration_nrpc_cross_lang_streaming` is missing from the explicit `--test` whitelist for the "CortEX + nRPC" nextest step. The file is `#[cfg(feature = "cortex")]`; nextest with `--test <name>` only runs binaries it's told about. The new cross-lang streaming compat test will compile but never run — exactly the silent-skip pattern the surrounding comment block warns against.

### H12 — Cross-language fixture lacks byte-exact wire snapshots
`net/crates/net/tests/cross_lang_nrpc/golden_vectors_streaming.json` (97 lines). Fixture only carries JSON request items + expected JSON responses. The B12 contract (NRPC_BINDINGS_PLAN.md:510) explicitly demands "canonical REQUEST + REQUEST_CHUNK + REQUEST_GRANT byte sequences" with "byte-exact wire output" assertions. Endianness, padding, header-ordering, varint, and flag-bit-layout regressions will pass this fixture across any binding that round-trips JSON correctly. The fixture cannot detect the very class of cross-binding drift it was created for.

### H13 — Cross-lang test is Rust-loopback, not cross-binding
`net/crates/net/tests/integration_nrpc_cross_lang_streaming.rs:50-579`. Caller-side `RpcClientPending::register_*` is wired straight to server-side `RpcStreamingRequestFold` in-process; no Node/Python/Go/C ABI is exercised. The file docstring (lines 14-16) and commit `f2b3947d` claim "cross-binding compat," but no other binding is involved. This is a Rust↔Rust handler conformance test mislabeled as cross-lang.

---

## MED

### M1 — `nrpc_streaming` bench still uses `debug_assert_eq`
`net/crates/net/sdk/benches/nrpc_streaming.rs:72`. Commit `e4cf19c2` only fixed `nrpc_duplex.rs`. Release benches strip `debug_assert!`, so a count mismatch in the server-streaming bench silently poisons throughput numbers — the exact failure mode that cubic-P2 fix targets.

### M2 — Wrong typed bad-request status code in streaming handler
`net/crates/net/sdk/src/mesh_rpc.rs:858` — `TypedStreamingRpcHandler` hardcodes `code: 0x4000`. The constant moved to `NRPC_TYPED_BAD_REQUEST = 0x8000` (line 63) to escape the reserved 0x0008..=0x7FFF canonical-status band. Unary path at line 802 uses the constant; the streaming path collides.

### M3 — `into_split` CANCEL-on-dual-drop is untested
`net/crates/net/tests/integration_nrpc_duplex.rs:312-348` (`duplex_into_split_lets_halves_run_independently`). Asserts the happy-path round-trip but does NOT verify the load-bearing claim that "CANCEL only fires when BOTH halves drop". Both halves are driven to clean completion. No negative test (one half drops early → CANCEL must NOT fire) and no positive test (both halves drop early → CANCEL MUST fire). The `Arc<DuplexInner>` drop refcount path at `mesh_rpc.rs:872` is uncovered for the acceptance criterion.

### M4 — No high-N round-trip test
Max N=10 in `sdk/tests/mesh_rpc_bidi_typed.rs` and N=10 in `tests/integration_nrpc_client_streaming.rs`. Phase E acceptance asks for 1000-chunk round-trip to surface credit-replenishment / channel-saturation / buffer-overflow bugs. Current N is well below the threshold where flow-control issues manifest.

### M5 — Racy DashMap check-then-remove on response delivery
`cortex/rpc.rs:3501-3505` (Unary delivery) and `:3525` (ClientStreaming delivery). Uses `self.senders.get` followed by `self.senders.remove`. Between them, a concurrent `cancel(call_id)` can vanish the entry; the `remove` returns `None`, the response is silently dropped, and the awaiting receiver hangs until its deadline. Use `DashMap::remove_if` or `alter` for atomic check-and-remove.

### M6 — Duplex CANCEL does not drain the response pump
`cortex/rpc.rs:1471` (`RpcDuplexFold::DISPATCH_RPC_CANCEL`). Removes `in_flight` + `senders` but does not wait for the response pump to exit. If the handler does not observe the cancellation token promptly it may keep emitting response chunks indefinitely, all of which the caller has already abandoned. Documented design choice (handler self-supervises) but worth flagging — a misbehaving handler turns CANCEL into a one-sided close.

### M7 — Per-frame GRANT cap is not a per-call cap
`mesh_rpc.rs:1177` and `:1417` (`grant_pump`). `sem.add_permits((credits as usize).min(usize::MAX >> 4))` clamps per frame, but a compromised target peer can over-grant up to `usize::MAX >> 4` worth of credits across many frames. Add per-call accumulated cap.

### M8 — Unbounded grant mpsc on the substrate
`cortex/rpc.rs:1646` — `register_client_streaming` returns `UnboundedReceiver<u32>`. A server publishing a million tiny grants can grow this queue without bound until `grant_pump` drains it. Bounded mpsc (e.g. 1024) with overflow drops would match `STREAMING_REQUEST_PUMP_CAPACITY` semantics.

### M9 — `Some(0)` rejection not propagated to response-side window
`mesh_rpc.rs:1114+` — fix in commit `905b945b` only covers `call_client_stream` and `call_duplex` request-direction. `stream_window_initial = Some(0)` on the response side still stalls the server's response pump forever on `acquire` instead of erroring at the public entry point. Add a symmetric guard or at least a doc note.

### M10 — `RpcDuplexFold` REQUEST_CHUNK arm duplicates `RpcStreamingRequestFold`
`cortex/rpc.rs:1418-1444` is a verbatim copy of the client-streaming arm. Future fixes (e.g. another meta/payload validation refinement) must be applied in two places. Refactor candidate.

### M11 — Go ctx-watcher goroutine leak defeats finalizer
`mesh_rpc.go:1316-1325` (CallClientStream) and twin in CallDuplex. A `context.WithCancel(parent)` whose Done is non-nil spawns a watcher; if the user forgets `Close()` AND ctx never cancels, the goroutine references `c` indefinitely. `runtime.SetFinalizer` cannot run because the goroutine keeps `c` live. Classic finalizer-defeated-by-goroutine leak.

### M12 — Go `spawn_blocking` task not joined on tokio timeout
`rpc-ffi/src/lib.rs` `GoClientStreamingRpcHandler::call` and the duplex twin. On `tokio::time::timeout` elapsed, the function returns `Err(...)` without joining/cancelling the Go dispatcher running in `spawn_blocking`. The substrate fold has already moved on; the still-running dispatcher's `Recv()`/`Send()` racing the timeout can touch torn-down state.

### M13 — No `cancellable` variant for streaming caller construction
`rpc-ffi/src/lib.rs` `net_rpc_call_client_stream` / `net_rpc_call_duplex` have no cancel-token parameter. `net_rpc_call_streaming_cancellable` exists for the older streaming path. A hung discovery / subscription setup wedges the caller's thread with no escape.

### M14 — Python duplex handler cannot raise Application errors
`net/crates/net/bindings/python/src/mesh_rpc.rs:629-633`. Python duplex handler `Err(pyerr)` always maps to `RpcHandlerError::Internal`. There is no `extract_app_error` call here, unlike the client-streaming path at `:559-565`. Asymmetric: Python duplex handlers cannot signal a typed Application status the way client-streaming handlers can.

### M15 — No `__aiter__`/`__anext__` on Python streaming types
`python/src/mesh_rpc.rs:199-232` (`PyDuplexCall`), `:376-410` (`PyDuplexStream`), `:462-492` (`PyRequestStreamRecv`). All use sync `__iter__`/`__next__` + `PyStopIteration`. asyncio consumers cannot `async for chunk in stream:` — they must call sync `next(stream)` which `block_on`s the runtime, fighting their event loop. `StopAsyncIteration` is never raised.

### M16 — `close()` cannot interrupt an in-flight `send()` (Node + Python)
- Node `mesh_rpc.rs:80-88, 154-162, 263-271`: `tokio::sync::Mutex` held across `.await` on the SDK send. A concurrent `close()` queues behind that send; users cannot abort a flow-controlled send waiting for credit.
- Python `mesh_rpc.rs:60, 77-87`: `std::sync::Mutex` + `take()` means a racing `close()` returns `None` and becomes a no-op while the in-flight `send` continues.

Both defeat `close()`'s purpose as an escape hatch.

### M17 — Cross-lang fixture matrix gaps
`golden_vectors_streaming.json` has no coverage of: `REQUEST_GRANT` flow-control frames, decode-failure / cancellation / `RpcStatus != Ok` error cases, both-flag-bits combinations on a non-initial chunk, `FLAG_END` on initial REQUEST with non-empty body. The integration test has dead code (`_suppress_unused`) explicitly marking the absent `error_cases` section as a known gap.

### M18 — 1-item vs N-item FLAG_END placement is undefined by the fixture
`integration_nrpc_cross_lang_streaming.rs:296-305, 403-410`. The 1-item case emits a trailing empty-body `FLAG_END` chunk; the multi-item case puts `FLAG_END` on the last item's chunk body. Two distinct send patterns under the same handler; no fixture case asserts which is canonical. A binding could emit either and the test would not notice.

### M19 — `NRPC_BINDINGS_PLAN.md` materially stale
`docs/misc/NRPC_BINDINGS_PLAN.md:25,341,510`. Status table claims `NET_RPC_ABI_VERSION = 0x0001` with C ABI `client-stream ❌ / duplex ❌`, but `include/net_rpc.h:106` already defines `0x0002` and exports all B8 caller-side functions. Open-questions section at `:24-25` is self-contradictory about the version bump.

### M20 — `NRPC_BIDI_STREAMING_PLAN.md` status header flipped but body still future-tense
`docs/plans/NRPC_BIDI_STREAMING_PLAN.md:11-32` shows all rows ✅ and appends a delivered-commits table, but `## Goal & scope` (line 42) and downstream sections still read as in-flight design ("Add three surfaces… will need streaming-aware mirrors"). Confusing for the canonical reference of "locked vs open."

---

## LOW

- **L1** — `cortex/rpc.rs:285`: `encode_request_grant` mirrors `encode_stream_grant`'s "credit big-endian, call_id little-endian" comment, but the rest of the codec is uniformly little-endian. Worth a comment audit in case the response-side legacy was a mistake.
- **L2** — `net_rpc.h:106`: ABI bumped to 0x0002 but no runtime enforcement helper. A 0x0001 binary dlopened against 0x0002 headers will only discover missing symbols at link time.
- **L3** — `net_rpc.h:154`: doc for `net_rpc_response_free` claims "idempotent on NULL or zero-length" — actively misleads consumers about the leak in H6.
- **L4** — Double-copy of every chunk body across Go/Node FFI (`C.CBytes` then `Bytes::copy_from_slice`). Python copy is necessary (GIL released); Node path could be zero-copy from `Buffer`.
- **L5** — `RpcStreamingContext` is silently dropped on server side in both Node (`node/src/mesh_rpc.rs:512, 584`) and Python (`python/src/mesh_rpc.rs:531, 605`). Peer, headers, and deadline are unavailable to JS/Python handlers — inconsistent with the unary path.
- **L6** — `cli/src/commands/rpc.rs:1-11`: header advertises four streaming subcommands but the router is a stub (`#![allow(dead_code)]`). Either hedge the title with "design stub" or ship the subcommands.
- **L7** — `parse_js_app_error` / `extract_app_error` use a stringly-typed `"nrpc:app_error:0x<code>:<body>"` contract with no regression test for codec-error collision on either binding.
- **L8** — `cortex/rpc.rs:1646` (`register_client_streaming`) returns `UnboundedReceiver<u32>` for grants; same observation as M8 from the substrate-API angle.
- **L9** — `JsResponseSink::send` recovers from poisoned `std::sync::Mutex` via `unwrap_or_else(|p| p.into_inner())`; defensive but undocumented for handlers running on multiple JS threads.
- **L10** — Node `JsRequestStream::next` serializes parallel `Promise.all([s.next(), s.next()])` through a mutex but doesn't warn users about ordering nondeterminism.

---

## Confirmations (no issue)

- `Some(0)` request-window rejection (commit `905b945b`) is correctly placed in the substrate (`mesh_rpc.rs:1775, 2021`) below every veneer entry point; the regression test pins it.
- `into_chunked()` correctly carries `seen_first` AND `done` across the conversion (commit `1cef02cd`); regression test at `mesh_rpc_bidi_typed.rs:451-519` covers post-partial-consume conversion.
- `DuplexCallTyped::poll_next` done-latching (commit `e7e69e2a`) correctly latches on decode-err, raw-err, AND substrate EOF (lines 1183, 1191, 1194); regression test at `mesh_rpc_bidi_typed.rs:359-437` pins the decode-error path.
- `RpcStreamTyped::poll_next` decode failure correctly produces one `Err` then EOF — no silent swallow.
- REQUEST_GRANT meta/payload `call_id` agreement validation (commit `6c3c810b`) is checked in both `apply_inbound` (rpc.rs:1922) and `apply` (rpc.rs:2008); regression test pins it.
- Node TSFN bridge: `oneshot::channel` with `let _ = tx.send(ret)` resolves exactly once; no double-resolve / never-resolve hazard for the bridge itself.

---

## Top three to fix before merge

1. **H5** — Go FFI duplex concurrent-use bug. Kills the primary duplex use case from Go.
2. **H1** — CANCEL drop-ordering race in the substrate. Silent response loss on every clean-drop in the field.
3. **H11 + H12 + H13** — CI silently skips the cross-lang streaming test, the fixture has no byte-exact snapshots, and the existing test is Rust-loopback. The cross-binding compat story is currently aspirational.

## Suggested follow-up batching

- **Batch A (substrate correctness)**: H1, H2, H3, H4, M5, M6, M7, M8, M9, M10.
- **Batch B (Go FFI safety)**: H5, H6, H7, H8, M11, M12, M13, L4.
- **Batch C (Node/Python parity)**: H9, H10, M14, M15, M16, L5.
- **Batch D (cross-lang conformance)**: H11, H12, H13, M17, M18.
- **Batch E (veneer + benches + docs)**: M1, M2, M3, M4, M19, M20, L1, L2, L3, L6, L7, L9, L10.
