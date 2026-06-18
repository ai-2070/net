# Bug Audit — 2026-06-18 — Bindings & Integration Edges

**Scope:** Full-workspace bug hunt across the `net` crate (~100k LOC Rust) plus the
Go / Python / FFI binding layers. Method: parallel per-subsystem audits, then
each reportable finding re-verified by hand against the source (file:line traced
end to end, pointer ownership and lock discipline checked).

**Headline:** The hardened Rust core is clean. Every concrete bug found lives at
the **language-binding / FFI edge**, not in the core protocol. Two are
use-after-free races in the *shipped* Go module (`github.com/ai-2070/net/go`),
reachable via ordinary context cancellation.

## Subsystems audited and found CLEAN (no concrete bug)

These came back with no actionable defect — they are saturated with prior-audit
regression tests and explicit fix annotations:

- Core mesh routing + event bus (`adapter/net/mesh.rs`, `bus.rs`, `mesh_rpc.rs`) — dedup/replay window, lock-across-await discipline, credit accounting, subscription GC, backoff overflow all verified correct.
- RedEX durable storage (`adapter/net/redex/{disk,file,segment,entry,index,retention,replication_*}.rs`) — torn-tail recovery, checksum survivor re-alignment, partial-write rollback, offset-u32 overflow, manifest pointer flip all correct. (One LOW TOCTOU below.)
- Identity / security (`adapter/net/identity/{token,entity,envelope,origin}.rs`) — ed25519 `verify_strict`, expiry, token-chain anchoring/continuity, sealed-box transport all sound.
- CortEX RPC + folded-state engine (`adapter/net/cortex/*`, `behavior/fold/*`, `redex/write_token.rs`) — RYW watermark lost-wakeup ordering, strict-prefix apply, write-token origin binding, LWW merge all correct. (One LOW watermark gap below.)
- Blob / Dataforts (`adapter/net/dataforts/blob/mesh.rs`) — blake3 re-verify on every fetch, range math, RS reconstruction, auth guards fail-closed.
- Behavior modules (`behavior/{capability,predicate,placement,deck,meshdb/planner}.rs`) — fail-closed capability checks, predicate boolean logic, placement scoring, causal-claim comparisons all correct.
- FFI memory-safety (`ffi/*`, `bindings/go/*-ffi`, `bindings/python/*`) — `HandleGuard` quiesce, `slice::from_raw_parts` length guards, alloc/free layout matching, panic-across-FFI catches all sound. (The *logic/lifecycle* bugs below are a different class than the memory-safety pass.)

---

## Findings

| # | Sev | Location | One-liner | Status |
|---|-----|----------|-----------|--------|
| 1 | HIGH | `go/mesh_rpc.go` + `bindings/go/rpc-ffi/src/lib.rs` | Streaming `Recv`/`Grant` UAF vs ctx-cancel watcher free | Verified |
| 2 | HIGH | `go/meshos.go` | `MeshOsDaemonHandle` method UAF vs `Free()`/finalizer | Verified |
| 3 | MEDIUM | `bindings/go/deck-ffi/src/lib.rs` | Stream-end reported as timeout for non-zero timeout | Verified |
| 4 | LOW | `deck/src/widgets/export.rs` | Exported timestamps print epoch hour-count (missing `% 24`) | Verified |
| 5 | LOW | `adapter/net/redex/replication_catchup.rs` | TOCTOU between `next_seq()` check and `append_batch` | Verified |
| 6 | LOW | `adapter/net/cortex/watermark.rs` | `app_seq` can fall behind a skipped corrupt same-origin event | Agent-reported, partly by-design |
| B-* | MED/LOW | `bindings/go/net/*` (divergent copy) | Hedge/watch/last-error bugs | See appendix |

---

### 🔴 HIGH 1 — Use-after-free: streaming `Recv`/`Grant` races the ctx-cancel watcher's free

**Files:** `go/mesh_rpc.go` (canonical module `github.com/ai-2070/net/go`) and `net/crates/net/bindings/go/rpc-ffi/src/lib.rs`

`RpcStream` (mesh_rpc.go:976) guards its C handle with only a `closed *atomic.Bool`
— **no mutex** — unlike `MeshRpc`, which routes every handle touch through an
RWMutex (`withHandle`). `Recv()` is check-then-use:

```go
func (s *RpcStream) Recv() ([]byte, error) {
    if s.closed.Load() { return nil, ErrStreamDone }   // 1153 — observes false
    var outChunk *C.uint8_t
    ...
    code := C.net_rpc_stream_next(s.handle, &outChunk, &outChunkLen, &outErr) // 1159 — uses s.handle
```

The ctx-cancel watcher goroutine spawned by `CallStreaming` frees concurrently:

```go
go func() {
    <-watchCtx.Done()
    if !closedPtr.Swap(true) {              // 1069
        C.net_rpc_stream_free(handlePtr)    // 1070
    }
    close(watcherDone)
}()
```

On the Rust side, `net_rpc_stream_free` deallocates the whole struct including its
**only `Arc`**:

```rust
pub extern "C" fn net_rpc_stream_free(stream: *mut RpcStreamHandleC) {   // lib.rs:1369
    if stream.is_null() { return; }
    unsafe { drop(Box::from_raw(stream)); }
}
```

while `net_rpc_stream_next` borrows `s = &*stream`, **takes** the inner stream out
of the mutex, blocks in `runtime().block_on(inner.next())`, and afterwards writes
back through the borrow:

```rust
let Some(s) = (unsafe { stream.as_ref() }) else { return NET_RPC_ERR_NULL };  // borrow into the box
...
let inner_opt = s.inner.lock().take();
let result = runtime().block_on(async { inner.next().await });   // parks here, holding &s
match result {
    Some(Ok(chunk)) => { *s.inner.lock() = Some(inner); ... }    // UAF if box was freed during block_on
```

**Interleaving** `Recv: closed.Load()==false → watcher: Swap+free → Recv: stream_next deref`:
the `&RpcStreamHandleC` borrow and the `Arc<Mutex>` it points to are dangling →
**use-after-free / memory corruption**. The watcher never nils `s.handle`, and
`block_on` parks for a potentially long time, so the window is wide.

**Reachability:** the documented happy path — `CallStreaming(ctx, …)` followed by
a `Recv()` loop, with `ctx` cancelled mid-recv. Not an exotic double-close. The
same shape exists in `CallServiceStreaming`, `ClientStreamCall.Send`, and
`DuplexCall.Send`/`Recv` (each carries only a bare `closed *atomic.Bool`, no mutex).

**Impact:** memory corruption / crash whenever a streaming call's context cancels
while a `Recv`/`Send`/`Grant` is in flight — a routine cancellation scenario.

**Fix:** give the streaming structs the same RWMutex discipline as `MeshRpc`:
`Recv`/`Send`/`Grant` take an RLock, recheck `handle != nil` under it, hold it
across the cgo call with `runtime.KeepAlive`; the watcher and `Close` take the
WLock before freeing. The `atomic.Bool` is insufficient because the free and the
use are *separate* cgo calls with no lock between them — a claim-then-use lock is
required, not a check-then-use flag. (A defense-in-depth complement on the Rust
side: clone the `Arc` at entry so the `Mutex` outlives a racing free — but that
does not save the initial `stream.as_ref()` deref, so the Go-side lock is the real
fix.)

---

### 🔴 HIGH 2 — Use-after-free: `MeshOsDaemonHandle` methods race `Free()` / finalizer

**File:** `go/meshos.go` (canonical module)

`NextControl`, `TryNextControl`, `PublishLog`, `GracefulShutdown`, `Metadata`,
`RefreshMetadata`, `PublishCapabilities` (meshos.go:494–543, 843–913) all do
**unsynchronized check-then-use** on `h.ptr` with no lock and no `runtime.KeepAlive`:

```go
func (h *MeshOsDaemonHandle) NextControl(timeoutMs uint64) (MeshOsDaemonControl, error) {
    if h == nil || h.ptr == nil { return MeshOsDaemonControl{}, ErrMeshOsInvalidArg }  // 508
    var out C.NetMeshOsDaemonControl
    if err := meshosStatusToError(C.net_meshos_next_control(h.ptr, C.uint64_t(timeoutMs), &out)); ... // 512 — BLOCKS
```

`Free()` frees the handle guarded only by `freeOnce` — which prevents double-*free*,
not free-vs-use:

```go
func (h *MeshOsDaemonHandle) Free() {
    if h == nil { return }
    h.freeOnce.Do(func() {
        if h.ptr == nil { return }
        ...
        C.net_meshos_handle_free(h.ptr)   // 561 — drop(Box::from_raw) on the Rust side
        h.ptr = nil                       // 562
        ...
    })
}
```

`net_meshos_next_control` is a **blocking** call (parks until the next event or
timeout). Two reachable triggers:

1. **Explicit concurrent `Free`** — `Free` is documented "Safe to call
   concurrently and repeatedly"; a natural pattern is to `Free` from another
   goroutine to unblock a parked `NextControl`. That frees the handle mid-call → UAF.
2. **Finalizer** — once `h.ptr` is passed into the cgo call, `h` can become
   unreachable; the finalizer (set at construction) fires `Free` while the C call
   is still running on the freed handle.

There is also a plain data race on the `h.ptr` field itself (read in the methods
without sync, written in `Free` without sync) — `go test -race` flags it. The
struct carries `pumpStop`/`pumpDone` but **no mutex on `ptr`**; the deck stream
types in the same module *do* carry a `mu sync.Mutex` for exactly this, so the
pattern is inconsistent.

**Impact:** memory corruption when a handle method runs concurrently with `Free`
or the finalizer.

**Fix:** add an RWMutex to `MeshOsDaemonHandle`; direct methods RLock + recheck
`ptr != nil` + hold across the cgo call + `runtime.KeepAlive(h)`; `Free` takes the
WLock before freeing.

---

### 🟠 MEDIUM 3 — Deck FFI reports stream-end as a timeout for any non-zero timeout

**File:** `net/crates/net/bindings/go/deck-ffi/src/lib.rs` — **systematic**, repeated at
lines ~761 (snapshot), ~878 (status-summary), ~1204, ~1303, ~1659 (log / failure /
audit streams).

```rust
let snap = runtime().block_on(async {
    if timeout_ms == 0 {
        inner.next().await
    } else {
        tokio::time::timeout(Duration::from_millis(timeout_ms), inner.next())
            .await
            .unwrap_or_default()        // 767 — collapses Err(Elapsed) AND Ok(None) to None
    }
});
match snap {
    Some(Ok(snap)) => { ... NET_DECK_OK }
    Some(Err(e))   => { ... NET_DECK_ERR_CALL_FAILED }
    None if timeout_ms == 0 => { ... NET_DECK_ERR_END_OF_STREAM }   // 793
    None           => { ... NET_DECK_OK }                          // 798 — also fires on genuine stream-end
}
```

`tokio::time::timeout(...)` returns `Result<Option<Result<T>>, Elapsed>`.
`.unwrap_or_default()` maps **both** `Err(Elapsed)` (timeout) **and** `Ok(None)`
(stream genuinely ended) to `None`. For any non-zero `timeout_ms`, the `None =>`
arm returns `NET_DECK_OK` with `*out = NULL` — so a **closed stream is
indistinguishable from a timeout**. `NET_DECK_ERR_END_OF_STREAM` is reachable only
when `timeout_ms == 0`, contradicting the doc ("On stream end returns
`NET_DECK_ERR_END_OF_STREAM`", lib.rs:736).

**Impact:** the idiomatic Go polling loop —
`for { item, err := s.Next(1000); if errors.Is(err, ErrDeckEndOfStream) { break }; if item == nil { continue }; ... }`
— **never terminates** after the substrate runtime shuts the stream down; it spins
`(nil, nil)` forever. Goroutine livelock.

**Fix:** match `Ok(None) → END_OF_STREAM` vs `Err(Elapsed) → OK/timeout`
explicitly instead of `unwrap_or_default()`, in all five stream functions.

---

### 🟡 LOW 4 — Exported LOGS timestamps print epoch hour-count, not a 24h clock

**File:** `net/crates/net/deck/src/widgets/export.rs:223-233`

```rust
fn format_ts_ms(ts_ms: u64) -> String {
    // Mirror the in-deck render format (HH:MM:SS.mmm) ...
    let total_sec = ts_ms / 1_000;
    let hh = total_sec / 3_600;        // 228 — missing `% 24`
    let mm = (total_sec / 60) % 60;
    let ss = total_sec % 60;
    let ms = ts_ms % 1_000;
    format!("{hh:02}:{mm:02}:{ss:02}.{ms:03}")
}
```

`ts_ms` is a Unix-epoch millisecond wall-clock. The renderer this function
documents itself as mirroring computes the hour as `(total_s / 3600) % 24`
(`tabs/mod.rs:137`). The export helper omits the `% 24`, so for a current
timestamp (~1.78e12 ms) `hh ≈ 494000` and the exported line reads e.g.
`494179:45:12.345` instead of `13:45:12.345`. Minutes/seconds/ms are correct;
only the hour field is wrong. Cosmetic — incorrect operator-facing export, no
crash/data-loss.

**Fix:** `let hh = (total_sec / 3_600) % 24;`, or call `tabs::fmt_ts_hms_ms`
directly so the two formats cannot drift again.

---

### 🟡 LOW 5 — TOCTOU between `next_seq()` validation and `append_batch` in replication catch-up

**File:** `net/crates/net/src/adapter/net/redex/replication_catchup.rs:447-477`

```rust
let local_next = file.next_seq();                  // 447 — takes+releases the state lock
if response.first_seq < local_next { return ... StaleChunk ... }   // 448
if response.first_seq > local_next { return ... GapBeforeChunk ... } // 454
// first_seq == local_next validated here
...
file.append_batch(&payloads)                       // 477 — re-acquires the state lock
    .map_err(...)?;
```

`apply_sync_response` validates `response.first_seq == local_next` (the replica's
tail), then in a *separate* lock acquisition calls `append_batch`, which assigns
brand-new contiguous seqs via its own `next_seq.fetch_add` — it does **not** use
the events' declared `event_seq`. If any other writer advances `next_seq` between
line 447 and 477, the replicated events land at seqs higher than their
leader-declared `event_seq`, silently breaking leader↔replica seq alignment (a
later chunk is then rejected as `GapBeforeChunk{divergence_suspected}` and the
`skip_to` machinery papers over a real misalignment).

**Why LOW:** the replication runtime is one task per channel, and a replica is not
expected to take direct local appends while following a leader. Reachable only if
an application appends directly to a replica-role file concurrently with
replication apply.

**Fix:** re-check `next_seq == expected_first_seq` and append under one held lock
(add a `RedexFile` method that does both atomically), or document the
single-writer invariant on replica-role files as load-bearing.

---

### 🟡 LOW 6 — `app_seq` watermark can fall behind a skipped corrupt same-origin event

**File:** `net/crates/net/src/adapter/net/cortex/watermark.rs:65-73` (and `adapter.rs:1127-1151`)
**Confidence:** agent-reported; partly by-design. Not independently deep-verified.

`WatermarkingFold::apply` returns early via `self.inner.apply(ev, state)?` *before*
the `app_seq` advance code. The inner fold returns a recoverable `RedexError::Decode`
on a corrupt/short same-origin event; under the default `FoldErrorPolicy::Stop`
the event is skipped (folded watermark advances, but `applied_through_seq` and
`app_seq` do not). Because `open` establishes correctness by awaiting
`wait_for_seq(next_seq - 1)` on the *folded* watermark, it returns even though the
last same-origin event was skipped, leaving `app_seq` below the claimed
`seq_or_ts`. After enough subsequent ingests, `app_seq.fetch_add(1)` can re-stamp
that `seq_or_ts`, yielding two wire events with the same per-origin sequence.

**Why LOW / partly by-design:** documented behavior is that an errored event is
skipped for both state and watermark; requires a corrupt same-origin event (disk
bit-rot past the 32-bit checksum) and manifests only as a collision after many
further ingests.

**Possible mitigation:** in `WatermarkingFold::apply`, parse the `EventMeta` and
advance `app_seq` via `fetch_max(seq_or_ts + 1)` for matching-origin events even
when the inner fold returns a recoverable `Decode` error (the header is
independently parseable and the wire slot is claimed regardless of body decode).

---

## Appendix — findings in the divergent `bindings/go/net/` copy

`net/crates/net/bindings/go/net/` is git-tracked but has **no `go.mod`** and is a
*superset* of the published `go/` module (it adds `resilience.go`, `tasks.go`,
`memories.go`, `transport.go`, `placement.go`). Whether these ship depends on
build wiring not confirmed here. The bugs below are real at the code level; treat
their impact as gated on that copy being built.

- **MEDIUM B-1 — `CallWithHedge` ignores `CancelLosers=false`** (`resilience.go:209-251`).
  `defer cancelRoot()` (line 210) fires on the success return, and every loser's
  `attemptCtx` descends from `rootCtx`, so losers are always cancelled the instant
  the winner returns — the opposite of the documented contract. *Fix:* when
  `CancelLosers==false`, derive loser contexts from the parent `ctx` (or a detached
  context), not `rootCtx`.

- **MEDIUM B-2 — `TasksWatch.Next(0)` + `Close()` deadlock** (`tasks.go:568-593`,
  `617-627`; same in `memories.go`). `Next(0)` blocks indefinitely in
  `net_tasks_watch_next` while holding `w.mu`; `Close()` also takes `w.mu` to free
  the cursor → both goroutines hang. *Fix:* snapshot the cursor under the lock,
  release it, then make the blocking call; or document that `Next(0)` must not be
  used concurrently with `Close()`.

- **MEDIUM B-3 — thread-local last-error read on a possibly-different OS thread**
  (`meshdb.go`, `meshos.go`, `deck.go` `wrap*Error` paths). The failing cgo call
  sets error detail on its current M's `thread_local!`; `wrap*Error` reads it via a
  *separate* cgo call with no `runtime.LockOSThread`, so Go may have rescheduled the
  goroutine onto a different M — intermittently losing or misattributing the error
  `Kind`/`Message`. Status codes stay correct; only the detail envelope is affected.
  *Fix:* bracket the status call and its `wrap*Error` read with
  `runtime.LockOSThread`/`UnlockOSThread`, or return detail via out-params.

- **LOW B-4 — `ControlEvents` pump busy-spins at 100% CPU** when the control
  channel closes (`meshos.go:1121-1155`): `NextControl(50)`'s 50ms is a recv
  *timeout*, not a sleep, so on a closed channel `recv()` returns `None`
  immediately → `continue` with zero delay. *Fix:* distinguish closed-channel from
  timeout in the FFI, or back off in the pump.

- **LOW B-5 — `ToolServeHandle.Close()` uses a non-atomic `bool`** (`tool.go:200-240`):
  two concurrent `Close()` calls race on `h.closed` (`-race` violation); downstream
  effects are individually idempotent so no UB, but the "second Close is a no-op"
  contract is violated under concurrency. *Fix:* make `closed` an `atomic.Bool`
  gated by `Swap`, like `ServeHandle`.

- **LOW B-6 — `already_shutdown` mis-bucketed as `ErrDeckCallFailed`** for
  null-returning reads (`deck.go` `Status()`, `BlastRadius()`): hardcodes
  `ErrDeckCallFailed` so `errors.Is(err, ErrDeckAlreadyShutdown)` is false even
  though the kind string is correct. *Fix:* route on the kind string.

- **LOW B-7 — `WaitForToken(token, 0)` silently degrades to a single non-blocking
  poll** (`tasks.go:430-441`, `memories.go:401-412`): `timeout_ms==0` means
  "poll, don't wait" for tokens but "block forever" for `WaitForSeq` — asymmetric
  and surprising. *Fix:* reject/clamp `0`, or document the asymmetry.

---

## Recommended order of action

1. **HIGH 1 + HIGH 2** — memory-corruption races in the consumed Go SDK, reachable
   via ordinary context cancellation. Apply the RWMutex + `KeepAlive` discipline
   (mirror the existing `MeshRpc.withHandle` pattern).
2. **MEDIUM 3** — deck-ffi EOF/timeout match; one-line-per-site fix, removes a
   consumer livelock.
3. LOW 4–6 and the appendix items as hygiene / when the bindings copy ships.
