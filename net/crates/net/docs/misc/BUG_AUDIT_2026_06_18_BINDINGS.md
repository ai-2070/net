# Bug Audit — 2026-06-18 — Bindings & Integration Edges

**Scope:** Full-workspace bug hunt across the `net` crate (~100k LOC Rust) plus the
Go / Python / FFI binding layers. Method: parallel per-subsystem audits, then
each reportable finding re-verified by hand against the source (file:line traced
end to end, pointer ownership and lock discipline checked).

**Headline:** The first pass found every concrete bug at the
**language-binding / FFI edge** — use-after-free races in the *shipped* Go module
(`github.com/ai-2070/net/go`), reachable via ordinary context cancellation. A
follow-up parallel pass (findings 7–18, six per-subsystem agents) added a **third**
binding UAF (`blob.go` `MeshBlobAdapter`, alongside `RpcStream` and
`MeshOsDaemonHandle`), a missing FFI panic-guard in `rpc-ffi`/`compute-ffi`, and a
handful of core-behavior logic bugs (reconcile eviction budget, aggregator
interval, load-balancer probe/ring) — so the Rust core is *mostly* clean but not
entirely.

A **third** parallel pass (findings 19–37, six per-subsystem agents over transport
core, RedEX, dataforts, behavior/fold, shard/crypto/identity, and nRPC/SDK) found
the first **core data-path** defects, narrowing the "core mostly clean" framing
further: a HIGH reliable-stream **sequence gap** in the canonical `mesh.rs` send
path (#19, hand-verified end to end) and a silent **OUTER-join row-drop** in the
meshdb executor (#20, hand-verified), plus a tier of behavior / replication /
FFI-edge mediums. One reported HIGH (crypto anti-replay `MAX_FORWARD`) was
investigated and **downgraded to informational** (#37) — the control is dead on
the hot path but the window math means it is *not* an exploitable replay bypass.

## Resolution (branch `bugfix/audit-2026-06-18`, 22 commits)

**Fixed + committed (34 of 37 findings + several appendix-class):**
- Rust core/behavior: #5, #6, #9, #10, #13, #14, #15, #18, #19, #20, #21, #22,
  #24, #25, #26, #27, #28, #29, #32, #33, #34, #35, #36. All compile-verified
  (`cargo check`) and the touched modules' `cargo test` pass; regression tests
  added per finding (existing tests that pinned buggy behavior — e.g. #24 ICE
  cooldown, #6 watermark — were updated).
- FFI / go-ffi Rust crates: #3, #8 (rpc-ffi + compute-ffi), #11, #16, #30, #31
  (rpc/compute/deck/meshos/meshdb-ffi), #4. Compile-verified; crate tests pass.
- Go bindings (canonical `go/` module): #1 (RpcStream/ClientStreamCall/
  DuplexCall), #2 (MeshOsDaemonHandle), #7 (MeshBlobAdapter) — the three
  use-after-free races. **Caveat:** this environment has no cgo C toolchain
  (`CGO_ENABLED=0`, no gcc), so these three are verified by `gofmt` + manual
  review only, not a cgo compile/link. The changes are mechanical RWMutex +
  `runtime.KeepAlive` additions mirroring the existing `MeshRpc.withHandle` /
  `MeshStream.Send` pattern.

**Investigated and intentionally NOT changed:**
- #37 — reverted. The "restore MAX_FORWARD in `commit`" hardening breaks 4
  existing replay-window tests that encode deliberate design: `commit` accepts
  large forward jumps so a receiver that missed >1024 packets survives heavy
  loss without a forced re-handshake (old counters are still caught by the age
  check). The audit already classified #37 as *not* an exploitable bug, so this
  is a behavior/policy change with a real reliability downside and no security
  gain — left to a deliberate decision.

**Deferred (not safely completable in this pass — see notes):**
- #23 — publish-path event-count/byte chunking. A non-trivial hot-path refactor
  (`send_on_stream`-style per-chunk credit/seq loop) in `mesh.rs`; deserves
  careful reliable-stream testing rather than a rushed edit. Still open: a
  `publish_many` of >2028 events panics `build_subprotocol`'s `assert!`.
- #17 — seed-pointer length validation. The real fix is a breaking C-ABI change
  (add `seed_len` to `net_compute_spawn`/`net_meshos_register…` + update Go
  callers + headers); disproportionate for a LOW finding only reachable by a
  caller violating the documented 32-byte contract (in-tree callers always pass
  32). The compute-ffi commit message overstates this — only #8/#31 landed there.
- #12 — `C.GoBytes(ptr, C.int(len))` ≥2 GiB truncation (~20 call sites). Each
  site needs bespoke error handling around `goBytesChecked`'s `(…, bool)`
  return; with no cgo toolchain to compile-verify, 20 blind edits is too risky.
- Appendix B-1..B-7 — in the divergent `bindings/go/net/` copy (+ B-4 pump
  busy-spin in canonical `meshos.go`); not addressed.

## Subsystems audited and found CLEAN (no concrete bug)

These came back with no actionable defect — they are saturated with prior-audit
regression tests and explicit fix annotations:

- Core mesh routing + event bus (`adapter/net/mesh.rs`, `bus.rs`, `mesh_rpc.rs`) — dedup/replay window, lock-across-await discipline, credit accounting, subscription GC, backoff overflow all verified correct.
- RedEX durable storage (`adapter/net/redex/{disk,file,segment,entry,index,retention,replication_*}.rs`) — torn-tail recovery, checksum survivor re-alignment, partial-write rollback, offset-u32 overflow, manifest pointer flip all correct. (One LOW TOCTOU below.)
- Identity / security (`adapter/net/identity/{token,entity,envelope,origin}.rs`) — ed25519 `verify_strict`, expiry, token-chain anchoring/continuity, sealed-box transport all sound.
- CortEX RPC + folded-state engine (`adapter/net/cortex/*`, `behavior/fold/*`, `redex/write_token.rs`) — RYW watermark lost-wakeup ordering, strict-prefix apply, write-token origin binding, LWW merge all correct. (One LOW watermark gap below.)
- Blob / Dataforts (`adapter/net/dataforts/blob/mesh.rs`) — blake3 re-verify on every fetch, range math, RS reconstruction, auth guards fail-closed.
- Behavior modules (`behavior/{capability,predicate,placement,deck,meshdb/planner}.rs`) — fail-closed capability checks, predicate boolean logic, placement scoring, causal-claim comparisons all correct.
- FFI memory-safety (`ffi/*`, `bindings/go/*-ffi`, `bindings/python/*`) — `HandleGuard` quiesce, `slice::from_raw_parts` length guards, alloc/free layout matching, panic-across-FFI catches all sound **in the core `ffi/*` crates**. (The *logic/lifecycle* bugs below are a different class than the memory-safety pass. **Second-pass exception:** `rpc-ffi`/`compute-ffi` carry no `catch_unwind`/abort-guard — see HIGH 8.)

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
| 7 | HIGH | `go/blob.go` | `MeshBlobAdapter` methods UAF vs `Close()`/finalizer (lock dropped before cgo call) | Verified |
| 8 | HIGH | `bindings/go/rpc-ffi`, `compute-ffi` | No `catch_unwind`/abort-guard — panic & `block_on` re-entry unwind across the C ABI (UB) | Verified (structural) |
| 9 | MEDIUM | `behavior/meshos/reconcile.rs` | Duplicate `RequestEviction` for one chain per tick (count arm skips `evicted_this_tick`) | Verified |
| 10 | MEDIUM | `behavior/aggregator/daemon.rs` | Zero `summary_interval` panics spawned task; comment falsely claims it is validated | Verified |
| 11 | MEDIUM | `ffi/cortex.rs` | 5 `(out_json,out_len)` fns skip the pre-zero contract (TIMEOUT path leaves stale out-param) | Agent-reported |
| 12 | MEDIUM | `go/{cortex,compute,netdb,blob}.go` | `C.GoBytes(ptr, C.int(len))` truncates/sign-flips payloads ≥ 2 GiB | Agent-reported |
| 13 | MEDIUM | `behavior/aggregator/daemon.rs` | `filter_novel` dedups on `fold_kind` only → multi-row summarizers re-publish every tick | Agent-reported (latent) |
| 14 | MEDIUM | `behavior/loadbalance.rs` | Half-open probe slot never released if caller skips `record_completion` | Agent-reported |
| 15 | LOW | `behavior/loadbalance.rs` | `add_endpoint` re-add leaks ~150 stale hash-ring vnodes | Agent-reported |
| 16 | LOW | `bindings/go/rpc-ffi/src/lib.rs` | `duplex_into_split` drops surviving half on partial-consume (premature CANCEL) | Agent-reported |
| 17 | LOW | `bindings/go/{compute,meshos}-ffi` | Unchecked 32-byte read from caller seed pointer (OOB read on short buffer) | Agent-reported |
| 18 | LOW | `adapter/net/redex/disk.rs` | `expected_entries * 8` can overflow before bounds check (32-bit only) | Agent-reported |
| 19 | HIGH | `adapter/net/mesh.rs` + `session.rs` | Reliable-stream seq consumed but not rolled back on scheduler backpressure → permanent gap + dup re-send | Verified |
| 20 | MEDIUM | `behavior/meshdb/executor.rs` | LEFT/RIGHT OUTER join drops preserved-side rows with a missing/non-scalar join key | Verified |
| 21 | MEDIUM | `adapter/net/redex/entry.rs` | Per-entry checksum covers payload only, not header → corrupt `seq` breaks `partition_point` reads | Agent-reported |
| 22 | MEDIUM | `behavior/meshdb/federated.rs` | Lost trailing `End` frame reports a fully-delivered result as `ExecutorError` | Agent-reported |
| 23 | MEDIUM | `adapter/net/mesh.rs` | `publish_to_peer` doesn't batch by event count → `assert!` panic on >2028 events | Agent-reported |
| 24 | MEDIUM | `behavior/meshos/ice.rs` | `ThawCluster` blocked by cluster-wide cooldown, contradicting the break-glass invariant | Agent-reported |
| 25 | MEDIUM | `behavior/deck.rs` | `AuditStream::poll_next` doesn't re-register a waker after a consumed tick → can park forever | Agent-reported |
| 26 | MEDIUM | `behavior/meshdb/transport.rs` | Duplicate in-flight `call_id` overwrites prior caller's response sender (debug_assert only) | Agent-reported |
| 27 | MEDIUM | `behavior/fold/routing.rs` | Route owner can't update its own route to a worse metric → stale route pinned until TTL | Agent-reported |
| 28 | MEDIUM | `behavior/meshos/reconcile.rs` | `MarkAvoid` re-emitted every tick — dedup guard reads `avoid_list`, never populated in prod | Verified |
| 29 | MEDIUM | `behavior/loadbalance.rs` | Hash-ring collision probe uses destructive `insert`, clobbers another node's vnode (refines #15) | Verified |
| 30 | MEDIUM | `bindings/go/rpc-ffi/src/lib.rs` | `write_response`/`find_service_nodes` write out-params with no null check (extends #8) | Verified |
| 31 | MEDIUM | `bindings/go/*-ffi` | No `len > isize::MAX` guard before `slice::from_raw_parts` (extends #8) | Verified (structural) |
| 32 | LOW | `behavior/meshdb/executor.rs` | `sort_merge_join` drops no-key rows on both sides; diverges from hash full-outer (latent) | Agent-reported |
| 33 | LOW | `behavior/loadbalance.rs` | Weighted-RR starves endpoints when all effective weights < 1.0 (ceil collapses to 1) | Agent-reported |
| 34 | LOW | `adapter/net/mesh_rpc.rs` | `mint_random_call_id` returns `0` on `getrandom` failure → concurrent calls evict each other | Agent-reported |
| 35 | LOW | `adapter/net/redex/replication_runtime.rs` | `OutstandingRequests` soft-cap only evicts expired → unbounded under sustained in-flight load | Agent-reported |
| 36 | LOW | `adapter/net/redex/retention.rs` | Age-based eviction assumes monotonic wall-clock; backward NTP step mis-counts drops | Agent-reported |
| 37 | INFO | `adapter/net/crypto.rs` | `commit()` doesn't enforce `MAX_FORWARD` on hot path — investigated, NOT an exploitable replay bypass | Verified (not a bug) |
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

## Findings — second pass (2026-06-18 follow-up)

A follow-up parallel audit (six per-subsystem agents over FFI memory-safety,
concurrency, wire parsing, Go cgo, and core behavior logic) added findings 7–18.
HIGH 1 was independently re-confirmed on both the Go and Rust sides during this
pass. Findings **7, 9, 10** were hand-verified against the source (file:line
traced, lock/ownership checked); finding **8** is structurally confirmed (the
panic catch is demonstrably absent); the rest are agent-reported with the
confidence noted.

---

### 🔴 HIGH 7 — Use-after-free: `MeshBlobAdapter` methods race `Close()` / finalizer

**File:** `go/blob.go` (canonical module `github.com/ai-2070/net/go`)

Same class as HIGH 1 / HIGH 2 — a **third** Go handle type with the lock-vs-free
hole, and the only one whose own doc comment claims it is already fixed. Every
method snapshots the handle and **releases the mutex before** the cgo call:

```go
func (a *MeshBlobAdapter) Store(blobRefBytes, data []byte) error {
    a.mu.Lock()
    handle := a.handle
    a.mu.Unlock()                 // 193 — lock dropped here
    if handle == nil { return ErrBlobClosed }
    ...
    rc := C.net_mesh_blob_adapter_store(handle, ...)   // 205 — C call runs unlocked
```

`Close()` frees under the same mutex, and the finalizer set at blob.go:169 also
calls it:

```go
func (a *MeshBlobAdapter) Close() error {
    a.mu.Lock(); defer a.mu.Unlock()
    if a.handle == nil { return nil }
    C.net_mesh_blob_adapter_free(a.handle)   // 180 — drop(Box::from_raw) on the Rust side
    a.handle = nil
    ...
}
```

A concurrent `Close()` — or the GC finalizer once `a` becomes unreachable
mid-call — frees the native adapter while `net_mesh_blob_adapter_store/fetch/...`
is in flight → dereference of freed Rust memory.

**Same pattern at:** `Store` (191-209), `Fetch` (218-240), `Exists` (246-265),
`PrometheusText` (271-283), `OverflowEnabled` (288-299), `OverflowActive`
(304-315), `OverflowConfig` (319-336), `SetOverflowEnabled` (340-…),
`SetOverflowConfig` (~364-376).

**Contradicts its own contract:** the struct doc (blob.go:122-125) states methods
"take an internal lock around `Close()` to serialize FFI `_free` against any
concurrent in-flight op" — the code does the opposite. Every sibling handle does
it right: `MeshStream.Send` (mesh.go:728-743) holds the RLock **across** the cgo
call precisely "so a concurrent Close/Shutdown can't race the native handles into
a use-after-free."

**Impact:** memory corruption / crash whenever a blob-adapter call runs
concurrently with `Close` or the GC finalizer.

**Fix:** make `mu` an `RWMutex`; hold the RLock across the cgo call (recheck
`handle != nil` under it) with `runtime.KeepAlive(a)`, and have `Close` take the
WLock before freeing — the same discipline prescribed for HIGH 1 / HIGH 2.

---

### 🔴 HIGH 8 — No panic guard in `rpc-ffi` / `compute-ffi`: panic & `block_on` re-entry unwind across the C ABI (UB)

**Files:** `net/crates/net/bindings/go/rpc-ffi/src/lib.rs` (`runtime()` 175-186;
raw `runtime().block_on(...)` at 747, 797, 1081, 1296, …) and
`net/crates/net/bindings/go/compute-ffi/src/lib.rs` (`runtime()` 157-168; raw
`rt.block_on(...)` at 310, 334, 1191, …)
**Confidence:** structurally confirmed (no `catch_unwind`/abort-guard present;
raw `Runtime::block_on`); a concrete triggering consumer is not constructed here,
so reachability is consumer-dependent.

Unlike the in-tree `cortex`/`mesh`/`blob` FFI (which wrap `block_on` with a
`Handle::try_current()` check that `process::abort()`s instead of panicking) and
unlike the sibling `meshos-ffi`/`deck-ffi`/`meshdb-ffi` (which wrap every body in
`ffi_guard!`/`catch_unwind`), these two crates have **no `catch_unwind` at any
entry point** and call tokio's raw `Runtime::block_on`. `Runtime::block_on` panics
("Cannot start a runtime from within a runtime…") when invoked from a thread
already inside a tokio runtime, and any other internal panic does the same — the
unwind crosses the `extern "C"` boundary into Go/cgo, which is undefined behavior.

This is the one second-pass finding that **narrows the first pass's CLEAN note**
("panic-across-FFI catches all sound"): that holds for the core `ffi/*` crates but
not for `rpc-ffi` / `compute-ffi`.

**Fix:** wrap every `extern "C"` body in `catch_unwind` (or the existing
`ffi_guard!`) and route `block_on` through the abort-on-reentry wrapper the
sibling FFI crates already use.

---

### 🟠 MEDIUM 9 — Duplicate `RequestEviction` for one chain in a single reconcile tick

**File:** `net/crates/net/src/adapter/net/behavior/meshos/reconcile.rs:226-234`

`reconcile` threads an `evicted_this_tick: HashSet<ChainId>` so "the Phase C
count-driven arm and the Phase D-1 scheduler arm don't both emit … for the same
chain in the same pass" (reconcile.rs:70-80). `diff_forced_evictions` runs first
and populates it (line 81); `diff_scheduler` honors it
(`evicted_this_tick.contains(&chain)`, line 455). But the count arm in
`diff_replicas` only **writes** the set, never reads it:

```rust
} else if actual_count > desired_count {
    if let Some(victim) = holders.and_then(|hs| hs.iter().next()).copied() {
        out.push(MeshOsAction::RequestEviction { chain, victim });  // 232 — no `evicted.contains` guard
        evicted.insert(chain);                                       // 233
    }
}
```

A chain that is both force-evicted (ICE) and over its desired replica count (this
node is leader) gets **two** `RequestEviction` actions in one tick — exactly what
the one-eviction-per-chain-per-tick budget exists to prevent.

**Fix:** gate the count-arm push with `if !evicted.contains(&chain)`, mirroring
`diff_scheduler` (reconcile.rs:455).

---

### 🟠 MEDIUM 10 — Zero `summary_interval` panics the aggregator background task

**File:** `net/crates/net/src/adapter/net/behavior/aggregator/daemon.rs:200`
(spawn); missing validation in `new` (164-186); `config.rs:94` `with_interval`
accepts any `Duration`.

```rust
pub fn spawn(self: Arc<Self>) -> JoinHandle<()> {
    let interval = self.config.summary_interval;
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);   // 200 — panics if interval == 0
```

`AggregatorDaemon::new` validates only `fold_kinds`;
`AggregatorConfig::with_interval` accepts `Duration::ZERO`.
`tokio::time::interval(Duration::ZERO)` panics ("interval period must be
non-zero"), killing the spawned task. The `health()` path even carries a comment
asserting "the validation at construction normally rejects this"
(daemon.rs:562-563) — **no such validation exists**.

**Fix:** reject `summary_interval.is_zero()` in `new()` (or clamp in `spawn`); fix
the misleading comment.

---

### 🟠 MEDIUM 11 — Five cortex FFI functions skip the documented out-param pre-zero contract

**File:** `net/crates/net/src/ffi/cortex.rs` — `net_memories_list` (~2643),
`net_tasks_snapshot_and_watch`, `net_memories_snapshot_and_watch`,
`net_tasks_watch_next` (~2041), `net_memories_watch_next`.
**Confidence:** agent-reported; the divergence from the documented contract and
the honoring siblings is verbatim-confirmed.

The module documents (cortex.rs:250-260) that every `(out_json,out_len)` fn must
leave `(null,0)` on any error return. Siblings (`net_tasks_list`,
`net_redex_tail_next`) call `zero_out_json(...)`; these five return early
(shutdown, filter-parse error, timeout) **without** zeroing. Worst on the
`watch_next` pair, whose un-zeroed paths include `NET_ERR_TIMEOUT` — a routine
poll-loop outcome — so a caller that reads the out-param can pick up stale stack
data and pass a garbage pointer to `net_free_string` (invalid free).

**Fix:** `zero_out_json(out_json, out_len)` on every early-return path in the five
functions.

---

### 🟠 MEDIUM 12 — `C.GoBytes(ptr, C.int(len))` truncates/sign-flips payloads ≥ 2 GiB

**Files:** `go/compute.go:529,666`, `go/cortex.go:295,570,877` (and watch pumps
362/643/939), `go/netdb.go:186`, `go/blob.go:239`.
**Confidence:** agent-reported.

```go
body := C.GoBytes(unsafe.Pointer(outPtr), C.int(outLen))   // size_t outLen cast through 32-bit C.int
```

A buffer ≥ 2 GiB sign-flips negative (cgo panics "negative length"); ≥ 4 GiB
truncates the copy. The package already has `goBytesChecked` (mesh_rpc.go:543) for
exactly this, but these byte/JSON read paths (large `ReadRange`, snapshot bundle,
multi-GiB blob fetch) bypass it.

**Fix:** route every large-payload read through `goBytesChecked` (size-validated
`size_t` → length).

---

### 🟠 MEDIUM 13 — `filter_novel` dedups on `fold_kind` only

**File:** `net/crates/net/src/adapter/net/behavior/aggregator/daemon.rs:326-335`
**Confidence:** agent-reported; latent (built-in summarizers emit one row per
kind).

```rust
let prev = latest.iter().rev().find(|s| s.fold_kind == summary.fold_kind);
match prev {
    None => true,
    Some(prev) => prev.source_subnet != summary.source_subnet || prev.buckets != summary.buckets,
}
```

`summarizer.rs` documents that custom impls may emit several `SummaryAnnouncement`
rows with the same `fold_kind` per tick (per-class / per-region rollups). The
baseline lookup picks the single most-recent buffered entry of that kind, so when
N>1 rows share a kind every row but one is diffed against the wrong baseline, looks
"novel", and is re-published every tick — defeating dedup and churning the capped
`latest` buffer.

**Fix:** key the baseline lookup on the row's identity (e.g. `source_subnet` /
class+bucket), not `fold_kind` alone.

---

### 🟠 MEDIUM 14 — Half-open circuit probe can be permanently claimed

**File:** `net/crates/net/src/adapter/net/behavior/loadbalance.rs:928`
**Confidence:** agent-reported.

Once `select()` returns `Ok(selection)` with `claimed_probe == true`, the
`half_open_probe` slot is cleared only by `record_completion` (via
`half_open_probe.swap(false)`). If the caller never calls `record_completion` for
that selection (dropped future, panic, lost result), the flag stays `true`,
`is_circuit_open` keeps returning `true`, and the recovered endpoint is silently
removed from rotation forever. The module's `ProbeGuard` doc acknowledges this
async-cancel hazard; the synchronous `select` path uses a bare bool + manual
release and relies on the caller pairing completion.

**Fix:** pair the probe claim with an RAII guard (or a watchdog) so a dropped
selection releases the slot.

---

### 🟡 LOW 15 — `add_endpoint` re-add leaks ~150 stale hash-ring vnodes

**File:** `net/crates/net/src/adapter/net/behavior/loadbalance.rs:768` (+ `add_to_hash_ring` ~1357)
**Confidence:** agent-reported.

`add_endpoint` for a `node_id` already present overwrites its `EndpointState` but
does **not** `remove_from_hash_ring(node_id)` first. `add_to_hash_ring` then
inserts ~150 fresh vnodes; where a hash collides with the node's own prior vnode,
the `while ... insert().is_some()` loop linear-probes to a new slot, stranding the
old vnode. Each re-add (endpoint reconnect / weight change) permanently leaks ~150
ring entries — inflating ring size and skewing distribution toward the re-added
node. It does **not** misroute (stale vnodes resolve to the same `node_id`); the
explicit `remove_endpoint` path is fine.

**Fix:** `remove_from_hash_ring(node_id)` at the top of `add_endpoint` before
re-inserting.

---

### 🟡 LOW 16 — `net_rpc_duplex_into_split` drops the surviving half on partial-consume

**File:** `net/crates/net/bindings/go/rpc-ffi/src/lib.rs:2025-2043`
**Confidence:** agent-reported.

After latching `done`, it unconditionally `take()`s both halves; in the
`(Some,None)` / `(None,Some)` arm it binds the surviving half then drops it on the
early `STREAM_DONE` return, firing a premature CANCEL/close on a half the caller
never received a handle for. Not memory-unsafe (Drop runs, no leak), but the call
is silently destroyed and unrecoverable.

**Fix:** on the partial-consume arm, put the surviving half back instead of
taking + dropping it.

---

### 🟡 LOW 17 — Unchecked 32-byte read from caller seed/hash pointer

**Files:** `compute-ffi/src/lib.rs:1160-1163` (`net_compute_spawn`), `1345-1348`
(`spawn_from_snapshot`); `meshos-ffi/src/lib.rs:773-775`, `861-863`.
**Confidence:** agent-reported.

```rust
let mut seed = [0u8; 32];
unsafe { std::ptr::copy_nonoverlapping(identity_seed, seed.as_mut_ptr(), 32); }
```

`identity_seed`/`seed_ptr` is only null-checked, never length-checked; the read
length is hard-coded to 32. A caller passing a shorter buffer triggers a 32-byte
OOB read. The 32-byte requirement is a doc-comment contract only — and in
compute-ffi there is no `catch_unwind` backstop (see HIGH 8). Same class as the
`read_hash` 32-byte read in `transport.rs:149-152`.

**Fix:** require callers to pass a length and validate it ≥ 32 before the copy.

---

### 🟡 LOW 18 — `expected_entries * 8` can overflow before the bounds check

**File:** `net/crates/net/src/adapter/net/redex/disk.rs:2162` (loop at 2171-2172)
**Confidence:** agent-reported; the overflow path is real, exploitability very low.

```rust
if bytes.len() < expected_entries * 8 { return Ok(None); }
...
let chunk: [u8; 8] = bytes[i * 8..i * 8 + 8].try_into().expect("8 bytes");
```

`expected_entries` is `index.len()` from the parsed `.idx` (1 entry / 20 disk
bytes). `expected_entries * 8` can overflow `usize`; a wrap to a small value would
pass the guard and let `i * 8` index OOB. Reaching it needs an exabyte-scale `.idx`
on 64-bit (impossible) or a ~10 GB index on a 32-bit build — and it's a local-disk
path, not network-untrusted.

**Fix:** `bytes.len() / 8 < expected_entries`, or `checked_mul`.

---

## Findings — third pass (2026-06-18 follow-up, transport / behavior / join deep audit)

A third parallel audit (six per-subsystem agents over the transport core,
RedEX persistence, dataforts, the behavior/fold layer, shard/crypto/identity, and
nRPC/CortEX/SDK) added findings 19–37. Findings **19, 20, 28, 29, 30** were
hand-verified against the source (file:line traced end to end, lock/ownership and
window math checked); **31** is structurally confirmed; the rest are agent-reported
with the confidence noted. Finding **37** is a reported HIGH that was investigated
and **downgraded** — it is recorded here for honesty, not as a defect.

Dataforts (blob refcount / LRU / gravity / overflow) and the shard/mapper +
crypto/identity/token + bus/event/timestamp core came back **CLEAN** this pass and
are not re-listed beyond the first-pass note above.

---

### 🔴 HIGH 19 — Reliable-stream sequence consumed but not rolled back on scheduler backpressure → permanent gap + duplicate re-send

**Files:** `net/crates/net/src/adapter/net/mesh.rs:11097-11119` (mid-batch flush;
same shape at the final-flush block 11125-11146) and
`net/crates/net/src/adapter/net/session.rs` (`try_acquire_tx_credit_inner`
399-403, `TxSlotGuard::drop` 509-523, `next_tx_seq` 1463-1466).
**Confidence:** Verified end to end.

`send_on_stream` allocates a sequence number **atomically with the byte credit**,
then builds, delivers, commits, and only afterwards registers the retransmit
descriptor:

```rust
let (guard, seq) = match session.try_acquire_tx_credit_matching_epoch(
    stream_id, stream.epoch, needed,
) {
    TxAdmit::Acquired { guard, seq } => (guard, seq),   // 11102 — seq consumed here
    TxAdmit::WindowFull => return Err(StreamError::Backpressure),
    TxAdmit::StreamClosed => return Err(StreamError::NotConnected),
};
let packet = builder.build(stream_id, seq, &current_batch, flags);
self.deliver_stream_packet(scheduled, &packet, peer_addr, stream_id)
    .await?;                                            // 11108 — early return BEFORE register_retransmit
guard.commit();                                         // 11109
Self::register_retransmit(&session, stream_id, stream.epoch, seq, &current_batch, flags); // 11110
```

The seq comes from `next_tx_seq()` — a bare `fetch_add` with **no rollback path**:

```rust
let seq = if admitted { Some(state.next_tx_seq()) } else { None };  // session.rs:399
// next_tx_seq(): self.tx_seq.fetch_add(1, Ordering::Relaxed)        // session.rs:1465
```

`TxSlotGuard::drop` refunds **only the credit bytes** — it never touches `tx_seq`:

```rust
impl Drop for TxSlotGuard {
    fn drop(&mut self) {
        if !self.active { return; }
        if let Some(state) = self.session.try_stream(self.stream_id) {
            if state.epoch() == self.epoch {
                state.refund_tx_credit(self.bytes);   // 520 — credit only, NOT the seq
            }
        }
    }
}
```

For a **scheduled** stream, `deliver_stream_packet` has a *second, independent*
backpressure source — a full FairScheduler queue — surfaced as the same
`Backpressure` error **after** the seq was consumed:

```rust
if self.router.scheduler().enqueue(queued) { Ok(()) }
else { Err(StreamError::Backpressure) }   // mesh.rs:11225 — packet was NOT enqueued
```

When this fires, the `?` at 11108 returns early: the guard drops and refunds credit
(correct), but `register_retransmit` never runs and the consumed `seq` is never
rolled back — and the packet was never put on the wire. `send_with_retry` then
**re-runs the whole `events` slice** with fresh sequence numbers
(`mesh.rs:11251-11257`).

**Impact (reliable stream):** a *permanent, unrecoverable gap* at the skipped seq —
the receiver records the next packet out-of-order, never advances `next_expected`
past the hole, and NACKs it forever; the sender's `on_nack(seq)` finds no
descriptor (never registered) and can't retransmit → eventual `failed` flag →
spurious `StreamReset`. Compounding it: any partial flush that *did* commit earlier
in the same call is **re-sent under new seqs** on retry → duplicate delivery. This
is the *documented* backpressure path under bulk load, not a rare edge.

**Fix:** allocate the sequence only after the packet is accepted (or make `seq`
refundable like the credit and unwind it on the failure path), and don't replay
already-committed events when `send_with_retry` re-enters after a partial-batch
backpressure.

---

### 🟠 MEDIUM 20 — LEFT/RIGHT OUTER join silently drops preserved-side rows with a missing join key

**File:** `net/crates/net/src/adapter/net/behavior/meshdb/executor.rs:994`
(reached via dispatch at 442-449; emit loop 490-503).
**Confidence:** Verified.

`build_hash_join_table` skips any row whose `JoinKeyMode::Field` key is
missing/non-scalar:

```rust
for row in rows {
    let Some(key) = try_encode_join_key(&row, key_mode) else {
        continue;                       // 994 — no-key build-side rows never enter the table
    };
    ...
    table.entry(key).or_default().push((row, false));
}
```

For `LeftOuter` the build side **is** the preserved (left) side
(`hash_join_one_sided(left_rows, right_rows, key_mode, true, false)`, line 443;
`RightOuter` is symmetric on the right, line 448). The `emit_unmatched_build` loop
only iterates rows that made it into `build`:

```rust
if emit_unmatched_build {
    for entries in build.into_values() {       // 491 — only rows that had a key
        for (b, matched) in entries {
            if !matched { out.push(encode_joined_row(/* (Some(b), None) */)?); }
        }
    }
}
```

So a left row with an absent/non-scalar join key is dropped entirely instead of
emitted with `right = None` — violating OUTER-join semantics (a NULL join key must
never *match*, but the row must still appear). `hash_join_full_outer` (518+)
handles the no-key case correctly, so this is also an internal inconsistency.
Reachable for any join on a JSON field that is absent in some rows. (The planner
rewrites `Field("origin"/"seq"/…)` to row-intrinsic modes that never fail, so this
only bites arbitrary JSON-field joins — hence MEDIUM, not HIGH.)

**Fix:** in the outer-join path, emit preserved-side no-key rows unmatched
(mirror `hash_join_full_outer`), rather than dropping them in
`build_hash_join_table`.

---

### 🟠 MEDIUM 21 — RedEX per-entry checksum covers the payload only, not the header

**File:** `net/crates/net/src/adapter/net/redex/entry.rs:182` (root cause);
recovery in `disk.rs:415-459`; consumed by `file.rs:1024,1087-1088,1139-1140,1176`.
**Confidence:** Agent-reported (high confidence on mechanism).

`payload_checksum(payload: &[u8])` hashes only the payload bytes — the 20-byte idx
record's header fields (`seq`, `payload_offset`, `payload_len`, `flags`) are **not**
covered. Recovery (`disk.rs::open`) validates each survivor only by re-checksumming
its payload and trims the dat tail by `payload_offset` monotonicity; it never
asserts `seq` is monotonically increasing. A torn/bit-rotted write that corrupts
the `seq` field while leaving payload+checksum intact is therefore accepted into the
recovered index. Every read (`tail`, `read_range`, `read_one`, `read_range_limited`)
uses `partition_point` assuming `state.index` is sorted by `seq`, so a non-monotonic
recovered index silently returns wrong/missing events; `next_seq = last.seq + 1` can
also jump or regress (a regressed `next_seq` on a leader re-issues already-replicated
seqs with different content → silent divergence). Bounded by requiring header-region
corruption specifically (payload corruption *is* caught) → MEDIUM.

**Fix:** extend the per-entry checksum to cover the header fields, or assert
`seq` monotonicity during the recovery walk.

---

### 🟠 MEDIUM 22 — Lost trailing `End` frame reports a fully-delivered federated result as failed

**File:** `net/crates/net/src/adapter/net/behavior/meshdb/federated.rs:1090-1114`;
interacts with `transport.rs:723,767`.
**Confidence:** Agent-reported.

`translate_responses` treats only `Batch { final: true }` as terminal; a successful
non-final batch that happens to be the last frame falls through to an
`ExecutorError { detail: "transport stream ended before terminal frame" }`.
`run_server_call` always sends intermediate batches with `final = false` and marks
only the residual flush terminal — so if the separate `End` send fails after a clean
batch flush, the caller sees all rows **plus** a spurious error and must discard the
whole result.

**Fix:** treat a clean stream end after a delivered batch as success, or make the
terminal marker ride the last data frame rather than a separate `End`.

---

### 🟠 MEDIUM 23 — `publish_to_peer` doesn't batch by event count → release-mode `assert!` panic

**File:** `net/crates/net/src/adapter/net/mesh.rs:8812` (path: `publish` 8324 /
`publish_many` 8335); assert in `pool.rs:260-266`.
**Confidence:** Agent-reported (high confidence).

```rust
builder.build_subprotocol(stream_id, seq, events, flags, 0)   // 8812 — entire slice, no chunking
```

`build_subprotocol` runs `assert!(events.len() <= NetHeader::MAX_EVENTS_PER_PACKET,
…)` (a real release-mode `assert!`; `MAX_EVENTS_PER_PACKET = 2028`). Unlike
`send_to_peer`/`send_routed`/`send_on_stream`, which all chunk at
`MAX_PAYLOAD_SIZE` (implicitly bounding event count), the publish path passes the
caller's whole slice straight through. `publish_many` with >2028 events panics the
calling task; a single payload between `MAX_PAYLOAD_SIZE` and 65535 bytes builds an
over-MTU packet the receiver drops silently.

**Fix:** chunk the publish path by event count and byte size like the unicast/stream
paths (or return an error instead of asserting).

---

### 🟠 MEDIUM 24 — ICE `ThawCluster` blocked by the cluster-wide cooldown (break-glass violated)

**File:** `net/crates/net/src/adapter/net/behavior/meshos/ice.rs:669`
(`cooldown_targets`), gate in `verify_commit` (~897).
**Confidence:** Agent-reported.

`cooldown_targets` maps both `FreezeCluster` and `ThawCluster` to
`CooldownTargets::ClusterWide`, and `check_ice_cooldown` runs before fold/verify, so
a `ThawCluster` arriving inside the (default 300s) window after a `FreezeCluster` is
rejected with `IceCooldownActive`. `event_loop.rs:1136-1137` explicitly states ICE
commits must "bypass by design — operators must be able to thaw the cluster
mid-freeze"; the freeze gate honors that, but the *separate* cooldown gate silently
blocks it. A test currently pins the broken behavior.

**Fix:** exempt `ThawCluster` from the cluster-wide cooldown.

---

### 🟠 MEDIUM 25 — `AuditStream::poll_next` doesn't re-register a waker after a consumed tick

**File:** `net/crates/net/src/adapter/net/behavior/deck.rs:1926-1935`.
**Confidence:** Agent-reported.

The empty-queue branch returns `Poll::Pending` with a comment claiming "no explicit
`wake_by_ref` needed," whereas the sibling `LogStream` (~2074) and `FailureStream`
(~2142) call `cx.waker().wake_by_ref()` in the identical branch. tokio's
`Interval::poll_tick` only registers the waker on its own `Pending` path; after
consuming a `Ready` tick and returning `Pending` with no record, no waker is
registered, so a bare `audit_stream.next().await` can wedge permanently. Masked in
tests by `tokio::time::timeout` wrappers and incidental wakes.

**Fix:** mirror the siblings' `cx.waker().wake_by_ref()` in the empty branch.

---

### 🟠 MEDIUM 26 — Duplicate in-flight `call_id` overwrites the prior caller's response sender

**File:** `net/crates/net/src/adapter/net/behavior/meshdb/transport.rs:392-412`.
**Confidence:** Agent-reported.

```rust
let prev = self.inflight.insert(call_id, InflightCaller { tx, .. });
debug_assert!(prev.is_none(), ...);   // release: prev silently dropped
```

In release builds a duplicate `call_id` drops the prior caller's `tx`; the first
caller's `ResponseStream` closes and surfaces a synthetic error / empty result with
no signal (the comment admits "the earlier caller would otherwise hang forever").
Mitigated because `call_id`s come from a process-global counter, so a collision
requires an id-recycling / hand-rolled-request bug a layer up → MEDIUM.

**Fix:** refuse the duplicate (return an error) rather than overwriting, even in
release.

---

### 🟠 MEDIUM 27 — Route owner cannot update its own route to a worse metric

**File:** `net/crates/net/src/adapter/net/behavior/fold/routing.rs:124`.
**Confidence:** Agent-reported (medium).

After the same-publisher anti-reorder gate, the metric gate runs unconditionally:

```rust
if incoming.payload.metric <= entry.payload.metric { Replace } else { Reject }
```

A same-publisher update with a strictly higher generation but a *worse* (higher)
metric — the owner re-announcing genuine link degradation — evaluates `3 <= 1` →
`Reject`. The owner's fresher observation is discarded and the table keeps
advertising the stale lower metric / old next-hop until TTL (~300s), delaying
convergence on link degradation. Marked medium/medium because it mirrors legacy
`RoutingTable` parity and may be a deliberate lowest-metric-wins choice.

**Fix:** allow a same-publisher, higher-generation update to replace regardless of
metric direction (an owner's newer announcement should win over its own older one).

---

### 🟠 MEDIUM 28 — `MarkAvoid` re-emitted every reconcile tick (`avoid_list` never populated in prod)

**File:** `net/crates/net/src/adapter/net/behavior/meshos/reconcile.rs:339`
(`diff_locality`).
**Confidence:** Verified.

The idempotence guard meant to emit one `MarkAvoid` per degraded peer reads
`actual.avoid_list`:

```rust
if actual.avoid_list.contains_key(&peer) { continue; }   // 339
```

But I grep-confirmed **every** `avoid_list.insert` in the `meshos` module is
`#[cfg(test)]` (reconcile.rs:644 is inside `#[test]
reconcile_with_no_daemon_intent_emits_nothing_even_with_state`; the rest are in
`state.rs`/`snapshot.rs` test fns). No production path populates `avoid_list` —
admin events only `clear`/`remove` it and the tick GC only prunes it. So the guard
never trips in the live loop and a persistently-degraded peer gets a fresh
`MarkAvoid` every tick → repeated dispatch + action-queue pressure. Tests pass only
because they pre-seed `avoid_list` manually.

**Fix:** have the `MarkAvoid` emission (or its application) actually record the peer
in `avoid_list` (with a TTL/decay), so the dedup guard becomes live.

---

### 🟠 MEDIUM 29 — Hash-ring collision probe uses destructive `insert`, clobbering another node's vnode

**File:** `net/crates/net/src/adapter/net/behavior/loadbalance.rs:1372`.
**Confidence:** Verified. **Refines LOW 15** (whose "linear-probes to a new slot,
stranding the old vnode" mechanism description is itself slightly off — the old
vnode is *overwritten*, not stranded).

```rust
while self.hash_ring.insert(hash, node_id).is_some() {   // 1372
    hash = hash.wrapping_add(1);
}
```

The comment (1361-1374) says the probe keeps every vnode distinct, but
`DashMap::insert` is **destructive** — it overwrites the slot and returns the old
value. On a true collision, slot `H` held `node_X`; this `insert` replaces it with
`node_id` (returns `Some(node_X)`), then probes `H+1` and inserts `node_id` *again*.
Net result: `node_X` loses its vnode and `node_id` occupies **both** `H` and `H+1`
— the exact ring skew the probe is meant to prevent. Practical impact is low (FNV-1a
over `u64` with ~150k entries makes collisions astronomically rare) and the loop
still terminates, but the primitive does the opposite of its stated intent.

**Fix:** probe with `contains_key` / `entry().or_insert` so the existing occupant is
preserved, e.g. `while self.hash_ring.contains_key(&hash) { hash =
hash.wrapping_add(1); } self.hash_ring.insert(hash, node_id);`.

---

### 🟠 MEDIUM 30 — Go `rpc-ffi` writes out-params with no null check

**File:** `net/crates/net/bindings/go/rpc-ffi/src/lib.rs:821-828` (`write_response`)
and `858-873` (`net_rpc_find_service_nodes`).
**Confidence:** Verified. **Extends HIGH 8** (same crate's missing-safety theme).

```rust
fn write_response(body: Vec<u8>, out_ptr: *mut *mut u8, out_len: *mut usize) {
    let boxed: Box<[u8]> = body.into_boxed_slice();
    let len = boxed.len();
    let ptr = Box::into_raw(boxed) as *mut u8;
    unsafe { *out_ptr = ptr; *out_len = len; }   // 825-828 — no out_ptr.is_null() guard
}
```

`write_response` is called on every success path (`net_rpc_call` 751,
`net_rpc_call_service` 801, `net_rpc_stream_next` …); `net_rpc_find_service_nodes`
similarly writes `*out_ptr`/`*out_count` unconditionally (858-861, 870-873). The same
file's `write_err` (275) *does* null-check `out_err`, and the core `ffi/mod.rs`
null-checks every out-param — so this is an inconsistency, not a deliberate contract.
A C/cgo caller passing NULL gets a null-pointer write / UB. The in-tree Go wrapper
always passes valid stack addresses (limiting real-world reachability → MEDIUM), but
the public ABI symbol is exposed to arbitrary `dlsym`/cgo callers.

**Fix:** null-check every out-param before writing, matching `write_err` and the core
`ffi/*` crates.

---

### 🟠 MEDIUM 31 — Go FFI crates: no `len > isize::MAX` guard before `slice::from_raw_parts`

**Files:** `bindings/go/rpc-ffi/src/lib.rs:268,738`; unguarded sibling sites in
`compute-ffi` (816, 881, 1034, …), `meshdb-ffi` (337, 961), `meshos-ffi` (419, 452),
`deck-ffi` (2552).
**Confidence:** Verified (structural).

```rust
Bytes::copy_from_slice(unsafe { std::slice::from_raw_parts(req_ptr, req_len) })  // 738
```

`from_raw_parts`'s safety contract requires `len <= isize::MAX`; the go-ffi crates
take `req_len`/`len` straight from C with only a null check. `grep isize::MAX` over
`bindings/go/**/*.rs` is empty, versus the core `ffi/mod.rs` (802, 856, 942, 1745)
which guards `if len > isize::MAX as usize { return … }` at every such site. Passing
`(size_t)-1` from cgo is immediate UB. MEDIUM because it requires an out-of-contract
length, but the core treats the guard as required defensive validation.

**Fix:** add the `len > isize::MAX` guard before every `from_raw_parts`/
`copy_nonoverlapping` in the go-ffi crates (and pair with HIGH 8's `catch_unwind`).

---

### 🟡 LOW 32 — `sort_merge_join` drops no-key rows on both sides

**File:** `net/crates/net/src/adapter/net/behavior/meshdb/executor.rs:1063`.
**Confidence:** Agent-reported; latent.

Both inputs are built via `filter_map(|r| try_encode_join_key(&r, key_mode).map(|k|
(k, r)))`, discarding no-key rows before merging, so *every* outer-join kind
(including FullOuter) loses preserved-side rows lacking a `Field` key — diverging
from `hash_join_full_outer` (#20's sibling). Latent because the planner hardcodes
`JoinStrategy::HashBroadcast` (planner.rs:692); SortMerge is reachable only via a
hand-built `OperatorPlan`.

**Fix:** same as #20 — carry no-key rows through and emit them unmatched on the
preserved side.

---

### 🟡 LOW 33 — Weighted-round-robin starves endpoints when all effective weights < 1.0

**File:** `net/crates/net/src/adapter/net/behavior/loadbalance.rs:1094-1095`.
**Confidence:** Agent-reported.

```rust
let total_ceil = (total_weight.ceil() as u64).max(1);
let target = (counter % total_ceil) as f64;
```

Two endpoints each with effective weight 0.5 (base 1 × `Degraded` 0.5×) give
`total_weight = 1.0` → `total_ceil = 1` → `target = 0` always; the cumulative loop
(`0.5 > 0`) always selects the first endpoint and the second is never chosen. Only
triggers with operator-set small weights (default 100 keeps `Degraded` at 50, clear
of this) → narrow.

**Fix:** scale weights into integer space before the modulus, or use a float-domain
selection that doesn't round the total down to 1.

---

### 🟡 LOW 34 — `mint_random_call_id` returns `0` on `getrandom` failure

**File:** `net/crates/net/src/adapter/net/mesh_rpc.rs:3848`.
**Confidence:** Agent-reported (high confidence on mechanism, very-low severity).

```rust
if getrandom::fill(buf).is_err() { return 0; }
```

Two concurrent calls that both mint `0` evict each other's pending entries
(`register(0, …)` overwrites), so the first caller's receiver gets `RecvError::Closed`
→ a spurious `RpcError::Transport` (not the clean timeout the doc claims), and a
stale 0-RESPONSE could resolve the wrong waiter (the S-4 `from_node` gate still
blocks cross-peer forgery; same-peer cross-call confusion is not). Only triggers when
OS entropy is unavailable (a near-fatal environment), and the cursor is left
exhausted so it self-heals when `getrandom` recovers.

**Fix:** latch a hard error on entropy failure, or reserve `0` as a sentinel and
re-mint.

---

### 🟡 LOW 35 — RedEX `OutstandingRequests` soft cap doesn't actually bound

**File:** `net/crates/net/src/adapter/net/redex/replication_runtime.rs:482-488`.
**Confidence:** Agent-reported.

`record` only GCs *expired* entries when at the cap:

```rust
if self.entries.len() >= REQUEST_REGISTRY_SOFT_CAP {
    self.entries.retain(|_, &mut inserted| now.saturating_duration_since(inserted) < REQUEST_TTL);
}
self.entries.insert(...);   // proceeds even if retain evicted nothing
```

If a replica legitimately has ≥256 in-flight requests all younger than the 30s TTL
(many channels / fast tick), `retain` evicts nothing and the insert still proceeds —
so the map grows unbounded until TTLs expire. Not a correctness bug (token matching
stays correct); purely a memory bound that doesn't bound as documented.

**Fix:** evict oldest-N (or refuse) when the cap is hit with no expired entries.

---

### 🟡 LOW 36 — Age-based retention assumes a monotonic wall-clock

**File:** `net/crates/net/src/adapter/net/redex/retention.rs:84-90`.
**Confidence:** Agent-reported (low).

```rust
for &ts in timestamps.iter() { if ts >= cutoff { break; } age_drop += 1; }
```

The age loop breaks at the first entry at/after the cutoff, treating the prefix as
"all older." Timestamps are wall-clock (`now_ns()`) captured at append time; a
backward clock step (NTP correction) can make a later entry carry a smaller
timestamp, so the early `break` yields a wrong drop count (retain-too-long or
over-evict). Affects retention accuracy only, not log integrity; the module already
documents the "~monotonic wall clock" assumption.

**Fix:** use the monotonic per-entry seq for age decisions, or scan fully rather
than breaking early.

---

### ⚪ INFO 37 — Reported anti-replay `MAX_FORWARD` bypass — investigated and downgraded (NOT a bug)

**File:** `net/crates/net/src/adapter/net/crypto.rs:523` (`commit`), reached via
`try_admit_rx_counter`/`update_rx_counter` (902-937).
**Confidence:** Verified — **not an exploitable replay bypass.**

A pass agent reported this as HIGH: `commit`'s forward branch (538-547) accepts any
forward jump and may zero the bitmap via `shift_bitmap_up` (shift ≥ 1024), while the
`MAX_FORWARD` cap is enforced only in `is_valid` (498-499) — and the perf-#132 hot
path (`try_admit_rx_counter` → `commit`) skips `is_valid` (the doc at 910-932 says so
explicitly). All of that is accurate. **However**, working through the window math:
when `commit` zeroes the bitmap it also sets `rx_counter = received + 1`, so the new
window bottom (`received − 1023`) is `≥ rx_counter_old` — *above every
previously-accepted counter*. No already-seen counter survives in the window, so none
can be re-accepted; each individual counter still commits at most once. The removal of
the `is_valid` pre-check therefore weakens a **dead operational / defense-in-depth
control** (large gaps that should force a re-handshake are now silently accepted with
a warn-log) but is **not** an exploitable replay bypass.

**Recommendation (hardening, not a fix for a live hole):** restore the `MAX_FORWARD`
reject inside `commit` itself so the documented "re-handshake past a large gap"
policy is enforced on the hot path, and correct the comment at 914-921 (which claims
`commit` "already rejects out-of-window … counters internally").

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

1. **HIGH 1 + HIGH 2 + HIGH 7** — three memory-corruption UAF races in the consumed
   Go SDK (`RpcStream`, `MeshOsDaemonHandle`, `MeshBlobAdapter`), all reachable via
   ordinary context cancellation / `Close` / GC finalizer. Apply one RWMutex +
   `KeepAlive` discipline to all three (mirror the existing `MeshRpc.withHandle` /
   `MeshStream.Send` pattern).
2. **HIGH 8** — add `catch_unwind`/abort-guard to `rpc-ffi` + `compute-ffi` so a
   panic or `block_on` re-entry can't unwind across the C ABI.
3. **HIGH 19** — reliable-stream sequence gap + duplicate re-send on scheduler
   backpressure (canonical `mesh.rs` send path). The only third-pass HIGH and the
   first confirmed *core data-path* corruption; fix seq allocation/rollback before
   the binding work if reliable scheduled streams are in use.
4. **MEDIUM 3 + 9 + 10** — deck-ffi EOF/timeout match (consumer livelock); reconcile
   double-eviction; aggregator zero-interval panic. Small, contained fixes.
5. **MEDIUM 20 + 21 + 22** — OUTER-join row-drop (silent query data loss), RedEX
   header-checksum gap (silent index corruption on bit-rot), federated lost-`End`
   spurious failure. Correctness of stored/queried data.
6. **MEDIUM 11–14, 23–31** — cortex out-param pre-zero, `GoBytes` truncation,
   `filter_novel` dedup key, half-open probe release; plus publish event-count
   batching (panic), ICE thaw cooldown (break-glass), audit-stream waker, dup
   `call_id`, route-owner degrade, `MarkAvoid` re-emit, hash-ring probe clobber, and
   the go-ffi null-out-param / `isize::MAX` guards (with HIGH 8).
7. LOW 4–6, 15–18, 32–36 and the appendix items as hygiene / when the bindings copy
   ships.
8. **INFO 37** — restore the `MAX_FORWARD` reject inside `commit` and fix the
   misleading comment (hardening; not a live vulnerability).
