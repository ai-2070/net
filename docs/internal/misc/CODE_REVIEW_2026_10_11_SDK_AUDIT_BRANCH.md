# CODE REVIEW 2026-10-11 — SDK-audit remediation branch (`LZL0/sdk-audit`)

**Scope:** the branch diff `master...fce8ec22e`, merge base
`3c7ec8875145b1232e79ce1a10bf19d3644d1b04`. 41 commits, 164 files,
+7983/−882. The branch remediates the findings of
[`SDK_AUDIT_2026_10_11.md`](SDK_AUDIT_2026_10_11.md) (batches 1–5).

**Method:** five parallel review lanes over disjoint slices — RUST (`sdk/`,
`src/`, `adapters/mcp/`), GO (`go/`, `bindings/go/`, `include/`), PY
(`bindings/python/`, `sdk-py/`), NODE (`bindings/node/`, `sdk-ts/`), BROWSER
(`browser-ts/`, `leaf/`). The coordinator re-read GO-1, PY-1, PY-2 and RUST-1
in source at `fce8ec22e`; all hold.

**Receipts:** source-established only. No `cargo` / `go test` / `pytest` /
`npm` / CI-script run backed this pass. Findings marked PLAUSIBLE are traced
but depend on a timing window or an engine behaviour not reproduced.

**Verdict: HOLD.** 22 findings: 7 HIGH, 8 MEDIUM, 7 LOW. Two are hard hangs
(PY-1, PY-2), one breaks every Go duplex split (GO-1), one reports a failed
migration as completed (RUST-1), one leaves audit High #3 half-fixed (RUST-2).

Status column: filled in by the repair pass (§ Repair log).

---

## HIGH

### GO-1 — `DuplexCall.Split` cancels the call it just split (CONFIRMED)

`go/mesh_rpc.go:2033-2042`. `Split` calls `d.guard.requestFree()` while its
own `guard.enter()` is still counted in flight, so `requestFree` sees
`s>>1 >= 1` and fires `guard.wake` (`:1118`), which issues
`net_rpc_cancel_call(token)`. `CallDuplex` always reserves a cancel token
(`reserveStreamCancel`), and the core cancel keep-alive lives on
`Arc<DuplexInner>` and survives `into_split` (`mesh_rpc.rs` ~2988/~7100), so
`pending.cancel(call_id)` pushes `StreamItem::Error(local_cancellation_payload())`
into the response channel. Every split `DuplexStream.Recv` — including the
typed duplex wrapper (`go/mesh_rpc_typed.go:662`) — sees a cancellation.
The only Split test covers the `ErrSplitWhileReceiving` refusal, never the
halves after a successful split.

### PY-1 — `Net.poll` / `Net.flush` ⇄ `Net.shutdown` lock-order deadlock (CONFIRMED)

`bindings/python/src/lib.rs:765-806`, `:866-893`. `poll` / `flush` now
`py.detach(...)` while holding `self.bus.read()`; on return they reacquire the
GIL with the read guard still held. `shutdown` (`:881`) / `__exit__` take
`self.bus.write()` while holding the GIL. Thread A in `poll` (I/O-bound) +
thread B in `shutdown` ⇒ A waits for the GIL holding the read lock, B waits
for the write lock holding the GIL. Made likely by the same branch moving
async `EventStream` polling to `asyncio.to_thread` (`sdk-py/.../stream.py:143`).

### PY-2 — `repr(A2aServeHandle)` self-deadlocks (CONFIRMED)

`bindings/python/src/a2a.rs:434-440`. `__repr__` evaluates
`self.inner.lock().is_some()` inside `format!` — the guard temporary lives to
the end of the statement — then calls `self.services()`, which locks the same
non-reentrant `parking_lot::Mutex`. Any `repr` / `print` / REPL display hangs
with the GIL held. `inner` became a mutex on this branch.

### RUST-1 — `cancel()` erases a recorded migration failure; `wait()` reports success (CONFIRMED, regression)

`sdk/src/compute.rs:2271-2292`. `cancel()` overwrites
`recent_failures[origin]` with `StateFailed("cancel requested")`, and on
`abort_migration` failure (no record) `remove`s the slot — deleting a genuine
failure the dispatcher recorded. Target rejects → record dropped, reason
recorded → user `cancel()` (gets `Err`) → reason gone → `wait_one_attempt`
reads "status None + no failure" and returns `Ok(())`. Minor variant: a
migration completing between the insert and the abort gets the fake cancel
reason.

### RUST-2 — device revocation only enforced by the MCP wrap gate (CONFIRMED, audit High #3 partial)

`sdk/src/revocation.rs:147`. Revoked subjects are checked only inside
`DelegationGate::verify` (`adapters/mcp/src/wrap/delegation.rs:265`).
`RevocationStore::apply_to` copies floors only and `DelegationChain::verify`
knows nothing of revoked subjects, so the documented provider path
(`RevocationStore::load(..).apply_to(&registry)` / Node+Python
`load_from_store`, then `chain.verify`) still accepts a revoked device's
root-issued grant until expiry. `net identity revoke` (CLI,
`cli/src/commands/identity.rs:384`) still writes only a floor.

### NODE-1 — GC of the `onStreamData` subscription object silently unsubscribes (CONFIRMED)

`bindings/node/src/lib.rs:1157`. The new `Drop for StreamDataSubscription`
closes the subscription when the JS wrapper is collected. The TSFN keeps the
handler alive but nothing keeps the wrapper alive, and no doc (`mesh.ts`
JSDoc, `sdk-ts/README.md:141`, web docs) says to retain it. On master
delivery lasted for the node's lifetime; now it ends at an arbitrary GC and
events silently fall back to `recv`.

### TS-1 — handler isolation only catches synchronous throws (CONFIRMED)

`sdk-ts/src/mesh.ts:733`, `sdk-ts/src/store-transport.ts:131`. `handler(data)`
is in a plain `try/catch` and its return value is ignored. An `async` handler
that rejects on a malformed peer payload is an unhandled rejection, which
crashes Node under the default `--unhandled-rejections=throw` — the exact
"peer payload crashes the host" case the fix was meant to close.

## MEDIUM

### GO-2 — the teardown wake does not reach a `Send` parked on flow-control credit (CONFIRMED)

`go/mesh_rpc.go:1118` + core `mesh_rpc.rs:2694` / `:3056`. A `Send` waiting
in `credit_sem.acquire_owned().await` is not woken by the substrate cancel
(`cancel_locally` signals only the response side). With `RequestWindow` set
against a slow server, `Close()` / ctx-cancel fires the wake but `Send` never
returns, `guard.leave()` never runs, the native handle is never freed, no
wire CANCEL is sent, and the goroutine leaks.

### GO-3 — new export missing from the shipped baseline and C surface record (CONFIRMED)

`net_rpc_request_stream_retire` was added to `exports.baseline` and
`net_rpc.h` but not to `bindings/go/net-ffi/exports.shipped.baseline` or
`tests/c_abi/SURFACE.md`. CI guards "shipped libnet exports match their
baseline" (`check-ffi-exports.py`) and the C5b surface-record check will fail.

### PY-3 — `MeshOS.next_control(timeout_ms=0)` never returns a queued event (CONFIRMED)

`sdk-py/src/net_sdk/meshos.py:362-367`. The slicing loop checks
`left_ms <= 0` before the first native call, so `timeout_ms=0` (and `1`,
via `int()` truncation) returns `None` with events queued. Previously a
zero timeout was a non-blocking receive.

### PY-4 — graceful-shutdown test passes only because its fake diverges from the native handle (CONFIRMED)

`sdk-py/tests/test_meshos_handle_locking.py:26-56`. The fake's
`graceful_shutdown` queues `Shutdown` and keeps the handle usable; native
`graceful_shutdown` (`bindings/python/src/meshos.rs:869-870`) `take()`s the
inner first, and the wrapper holds `_borrow_lock` for the whole grace
period. A parked daemon therefore never observes `Shutdown` in the real
binding; the test asserts the opposite.

### BROWSER-1 — `pendingBytes` never reset when a consumer drains the buffer (CONFIRMED)

`browser-ts/src/stream.ts:246`, `:256` vs `:301-311`. `onMessage` and the
async iterator `splice` out `pending` without zeroing `pendingBytes`. After a
drain → detach → rebuffer cycle the stale count trips the byte cap almost
immediately and `receive` evicts all but the newest frame on nearly every
arrival (visible only via `droppedBeforeConsumer`).

### NODE-2 — `serveEnrollment` `approveTimeoutMs` is a raw u32 (CONFIRMED)

`bindings/node/src/enrollment.rs:763-769`. napi coerces `-1` to
4 294 967 295 ms (~49.7 days), and `0` is a zero budget that denies every
request — inconsistent with this branch's "`0` disables the cap" convention
elsewhere (blob, publish, nRPC) and with `validate_ttl_seconds`'s f64
validation.

### RUST-3 — subscribe/unsubscribe race between sibling daemons (PLAUSIBLE)

`sdk/src/compute.rs:1279`, `:1900`. `subscribe_channel` awaits the wire join
before recording it in the daemon ledger; `subscription_held_elsewhere` only
reads ledgers. Daemon B's join acked-but-unrecorded + daemon A's unsubscribe /
`stop()` ⇒ node-level leave while B believes it is subscribed.

### RUST-4 — daemon `stop()` can drop host-app channel subscriptions (PLAUSIBLE, newly widened)

`sdk/src/compute.rs:1065-1080`. `stop()` now runs `teardown_subscriptions`,
which only consults other daemons' ledgers, not channels the host app joined
directly via `mesh.inner().subscribe_channel` on the same node.

## LOW

### BROWSER-2 — `reconnect()` inherits an in-flight open's rejection (CONFIRMED, narrow)

`browser-ts/src/store/join.ts:791-799`. `opening = null` was removed; a
pending open from the lost session that later rejects (non-stale) fails
`reconnect()`; one that never settles hangs it.

### BROWSER-3 — Web Lock name hashes the secret's raw case (CONFIRMED)

`browser-ts/src/node.ts:1271`. `SECRET_HEX` accepts both cases and the leaf
decodes them to one identity, but `"AB…"` and `"ab…"` produce different lock
names — two tabs, one identity, mutual eviction.

### GO-4 — `Recv` racing a backed-out `Split` reports a clean `ErrStreamDone` (CONFIRMED)

`go/mesh_rpc.go:1979-1983`, `:2023-2026`. Interleaved, `Recv` sees
`splitting` and returns `ErrStreamDone` while `Split` sees `recvActive` and
backs out; the call is live but the reader was told it ended.

### TS-2 — `parseJsonLossless` assigns `__proto__` (CONFIRMED)

`sdk-ts/src/_internal.ts:203`. `obj[key] = value()` with `key === "__proto__"`
swaps the prototype instead of creating an own property as `JSON.parse` does.

### TS-3 — deck u64 fields are `number` or `bigint` by magnitude (PLAUSIBLE)

`sdk-ts/src/deck.ts` (all `parseJsonLossless` sites). `JSON.stringify(deck.status())`
throws on almost every snapshot (random node ids exceed 2^53), and
`record.nodeId === mesh.nodeId()` is type-mismatched for small ids.

### BROWSER-4 — gathering completion without a ≥700 error now reads `notRun` (PLAUSIBLE, design)

`browser-ts/src/udp-probe.ts:103-110`, `:245-256`; `leaf/src/bootstrap.rs:657-669`.
An engine that never fires `icecandidateerror` and completes gathering within
the deadline never yields `udp-blocked` on a UDP-blocking network.

### RUST-5 — `call: call: …` doubled prefix (cosmetic)

`SdkError::Rpc` Display adds its own `call: ` prefix; `mesh_enroll.rs` error
strings already carried one.

---

## Checked and sound

Go in-flight cancel ordering, server handle retire gate, meshos v2 `armed`
double-free guard, compute-ffi Arc-clone-then-consume; PyO3 stream close
latching, `NetMesh` node lock, enrollment approver fail-closed; TS/Py
filtered-cursor adoption, watch-iterator close; browser Web Lock
acquire/release, follower org deadlines, per-peer handle cap, lobby decimal
ids; Rust breaker, delegation-gate ordering, token TTL clamp, blob cap,
`forget` refusal, own-org floor rule.

---

## Repair log

| ID | Status | Commit |
|----|--------|--------|
| GO-1 | FIXED — `Split` uses `requestFreeQuiet`; test fails with the old call (`nrpc:cancelled … after Split`) | `e38414477` |
| GO-2 | FIXED (core) — the stream cancel watcher closes the credit semaphore; parked `send` returns `Cancelled`; two core tests, both RED without the fix | `e38414477` |
| GO-3 | FIXED — shipped baseline + `SURFACE.md` updated; `check-ffi-exports` / `c-surface-record --check` pass locally | `e38414477` |
| GO-4 | FIXED — `Recv` waits out a Split under way (no native test: the race needs a scheduler hook) | `e38414477` |
| PY-1 | FIXED — bus behind its own `Arc`, guard dropped before `detach`; `test_net_poll_shutdown.py` | `57152af8b` |
| PY-2 | FIXED — one lock in `__repr__`; live repr test | `57152af8b` |
| PY-3 | FIXED — always one native call | `57152af8b` |
| PY-4 | FIXED — core `signal_shutdown` / `is_registered`, PyO3 `signal_shutdown` / `registered` / `close`, wrapper waits the grace without the lock; fake now mirrors native consumption | `57152af8b` |
| NODE-1 | FIXED (SDK) — `MeshNode` holds each subscription until `close()` / shutdown; raw napi doc says to hold it | `d645b38f3` |
| NODE-2 | FIXED — `approveTimeoutMs` validated as a whole f64 in `1..=u32::MAX`; Python twin refuses `0` | `d645b38f3` |
| TS-1 | FIXED — `callIsolated` routes sync throws and rejections | `3be9aba13` |
| TS-2 | FIXED — `__proto__` becomes an own property | `3be9aba13` |
| TS-3 | DOCUMENTED — number-or-bigint by magnitude is inherent to lossless parse of untyped JSON | `3be9aba13` |
| BROWSER-1 | FIXED — `drainPending()` zeroes the count | `3ecf60c10` |
| BROWSER-2 | FIXED — open epoch detaches a lost session's in-flight open | `3ecf60c10` |
| BROWSER-3 | FIXED — lock name hashes the lowercased secret | `3ecf60c10` |
| BROWSER-4 | OPEN — design trade-off; needs a Firefox/Safari `icecandidateerror` check before changing | — |
| RUST-1 | FIXED — reason recorded with `or_insert` under the lock that spans the abort; never removed | `b36695848` |
| RUST-2 | FIXED — revoked subjects in core `RevocationRegistry`; `apply_to` carries them; CLI `--subject` | `b36695848` |
| RUST-3 | FIXED — runtime membership lock serializes join+record against leave | `b36695848` |
| RUST-4 | DOCUMENTED — the runtime cannot see host-app subscriptions (core keeps no per-node record) | `b36695848` |
| RUST-5 | FIXED | `b36695848` |

Found during the repair pass: `sdk-ts/test/mesh_node_surface.test.ts` was
failing on the branch (the approver-gated `serveEnrollment` forwarder was
missing from its table) — fixed in `3be9aba13`.
