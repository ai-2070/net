# Code review — `sdk-scheduler` branch

**Date:** 2026-06-25
**Branch:** `sdk-scheduler`
**Base:** `master`
**Scope:** 32 files, +5,359 / −24 LOC, 30 commits ahead.
**Plan:** [`SDK_SCHEDULER_TASK_LIFECYCLE_PLAN.md`](../plans/SDK_SCHEDULER_TASK_LIFECYCLE_PLAN.md)

The branch surfaces two existing core features — the gang-claim GPU-island
scheduler and the cortex workflow task-lifecycle (`WorkflowAdapter`, shards,
triggers) — across every binding layer: Rust SDK → napi / PyO3 / C-ABI → TS /
Py / Go / C. It is **pure surfacing**: no new core semantics, the bindings
re-export / wrap and the caller applies the results.

---

## Overall assessment

The Rust SDK core (`sdk/src/gang.rs`, `sdk/src/cortex/workflow.rs`, the `Mesh`
methods), the napi / PyO3 bindings, and the C FFI for the workflow + gang
surfaces are clean and idiomatic:

- consistent `HandleGuard` quiescing on the async-holding handles
  (`RedexHandle` / `WorkflowAdapterHandle`), with the inner in `ManuallyDrop`
  and the box intentionally leaked — the established audit-#23 recipe;
- correct two-pass buffer capping on the **read-only** list calls
  (`net_workflow_subtree`, `net_mesh_match_gpu_islands`, `net_workflow_try_join`
  failed-ids), with `copy_nonoverlapping` type-correct because `IslandId` and
  `TaskId` are both `u64` aliases;
- a `TriggerEngineHandle` that clones `Arc<WorkflowAdapter>` so it correctly
  outlives a freed `WorkflowAdapterHandle`;
- consistent lock ordering (`state.read()` → `results.lock()` → `engine.lock()`)
  across the FFI / napi / PyO3 trigger paths — no inversion, no deadlock;
- panic-safety is fine: the entry points are plain `extern "C"` (not
  `C-unwind`), so a panic aborts at the boundary (defined behavior), and the
  release profile is `panic = "abort"` (`Cargo.toml:503`).

The napi / PyO3 / TS / Py layers are test-verified (✅ in the parity matrix:
vitest, pytest, Rust surface tests).

**The defects are concentrated in the Go/C layer**, which the parity matrix
itself marks 🟡 ("compiles, runtime CI-only") and which ships with **no Go test
for the scheduler** (`go/*_{scheduler,workflow,gang}_test.go` do not exist). One
is a high-severity functional bug. All findings share a single root cause: the
C-ABI two-pass `(out_buf, cap, out_count)` out-buffer convention was applied to
operations that either **mutate** state or are **not atomic** across the two
calls.

---

## Findings

### F1 (High) — Go `OnTick` / `OnTaskChange` silently consume triggers and always return empty

`go/cortex.go:1534` (`OnTaskChange`) and `go/cortex.go:1575` (`OnTick`) use the
two-pass "size with NULL buffers, then fill" convention — but the underlying
engine calls are **destructive, not idempotent reads**:

- `TriggerEngine::on_task_change`
  (`src/adapter/net/cortex/workflow/trigger.rs:241`) does
  `self.by_task.remove(&task)`, fires the satisfied triggers, and re-inserts
  only the still-armed ones — **fired triggers are disarmed**.
- `TriggerEngine::on_tick` (`trigger.rs:263`) does `split_off` and drains the
  due `tick <= now` prefix — **fired ticks are disarmed**.

The FFI (`src/ffi/cortex.rs:702`, `:763`) calls `engine.lock().on_tick(&world)`
/ `on_task_change(...)` **unconditionally**, regardless of whether the
out-buffers are NULL. So the Go flow is:

1. **Pass 1** (NULL buffers, to learn the count) fires + disarms the triggers
   and **discards** the actions.
2. **Pass 2** finds nothing armed → returns `count = 0`.

**Effect:** Go's `OnTick` / `OnTaskChange` always return an empty slice and the
fired actions are lost. The Tier-2 trigger feature is non-functional on Go —
dependent tasks silently never get submitted/started, so workflows stall with
no error.

**Blast radius:** Go (C-ABI two-pass) only. napi (`bindings/node/src/cortex.rs:2765`)
and PyO3 (`bindings/python/src/cortex.rs:3942`) call the engine **once** and
return the `Vec` directly, so they are correct. The C example
(`examples/scheduler.c`) does not exercise triggers, and there is no Go test, so
nothing catches this.

**Fix:** these consuming calls cannot be sized first. Size the buffer from
`armed_count()` (a valid upper bound: fired ⊆ armed) and make a **single** call:

```go
func (e *TriggerEngine) OnTick(now uint64) ([]TriggerAction, error) {
    n, err := e.ArmedCount()        // upper bound; fired ⊆ armed
    if err != nil { return nil, err }
    if n == 0 { return nil, nil }
    kinds := make([]C.int, n)
    ids := make([]uint64, n)
    var count C.size_t
    code := C.net_trigger_on_tick(e.handle, C.uint64_t(now),
        &kinds[0], (*C.uint64_t)(unsafe.Pointer(&ids[0])), C.size_t(n), &count)
    // ... map min(count, n) entries
}
```

(Same shape for `OnTaskChange`.) Add a Go `*_test.go` that arms
`AtTick` / `AfterTask` and asserts the actions actually come back.

### F2 (Medium) — the C header documents the consuming trigger calls like the idempotent ones

`net_trigger_on_task_change` / `net_trigger_on_tick` in `include/net_cortex.h`
(and the vendored `go/net_cortex.h`) carry the same
`(out_kinds, out_ids, cap, out_count)` shape as the genuinely two-pass-safe
`net_workflow_subtree` / `net_workflow_snapshot`, with **no note that they
fire-and-consume on every call**. A pure-C user will write the same broken
two-pass loop F1 describes.

**Fix:** document them as single-shot consuming calls (caller passes a buffer
sized to `armed_count` up front), or — immune to the whole class — have them
return an owned heap buffer freed via `net_free_string`, like the JSON surface.

### F3 (Medium) — Go `Snapshot()` can return a truncated / corrupt buffer under concurrent writes

`go/cortex.go:1282`: the two-pass snapshot handles the *shrink* case
(`if int(length) < len(buf)`) but not *growth*. Transitions take
`w.mu.RLock()` (via `seqOp`), and `Snapshot()` also holds only `RLock`, so a
concurrent transition can append between pass 1 (sizes `buf` to `L1`) and pass 2
(now serializes `L2 > L1`; the FFI at `src/ffi/cortex.rs:844` copies
`min(L2, cap=L1) = L1` bytes and sets `out_len = L2`). The grow branch is never
taken, so Go returns `L1` bytes of an `L2`-byte serialization — a corrupt
snapshot that fails to restore via `open_from_snapshot`.

**Blast radius:** Go only; napi / PyO3 return the buffer in one call. The id-list
two-pass calls (`Subtree`, `MatchGpuIslands`, `TryJoin`-failed) only yield a
valid-but-incomplete list on growth, so they are benign by comparison.

**Fix:** loop — if `out_len > len(buf)` after pass 2, re-allocate and retry.

### F4 (Low) — `TriggerEngineHandle` / `ShardGroupHandle` omit the `HandleGuard`

Every other concurrently-shared handle in `src/ffi/cortex.rs` (`RedexHandle`,
`RedexFileHandle`, `WorkflowAdapterHandle`) carries a `HandleGuard` so a `_free`
racing an in-flight call degrades to a clean `ShuttingDown` instead of a UAF
(audit #23). The two new Tier-2 handles (`src/ffi/cortex.rs:515`, `:390`) free
with a bare `Box::from_raw`. The Go wrapper masks this (finalizer-only free + the
object stays reachable during a call), but a direct C consumer — or any future
explicit-free API — gets UB where the siblings get an error code.

**Fix:** add a guard for consistency, or a comment explaining why these two are
exempt.

### F5 (Nit) — assorted consistency gaps

- `net_mesh_claim_gpu_island` (`src/ffi/mesh.rs:3839`): on the `found == 0` path
  it leaves `*out_island` untouched rather than pre-zeroing it, unlike the
  pre-zero discipline elsewhere in the FFI. Contract says don't read it, so
  harmless — but inconsistent.
- Go `ShardGroup` has no public `Free()` (only a finalizer, `go/cortex.go:1369`),
  while `WorkflowAdapter` exposes `Free()` — shard groups can't be released
  promptly. `ShardGroup.free()` / `TriggerEngine.free()` (`go/cortex.go:1486`)
  also lack the mutex+nil guard `WorkflowAdapter.free()` has (unreachable in
  practice given finalizer-only invocation, but inconsistent).

---

## Recommendation

F1 is a ship-blocker — Go triggers are silently broken and untested. F2 and F3
are worth fixing in the same pass since they share F1's root cause. F4/F5 are
consistency cleanups. Everything on the Rust / napi / PyO3 side is solid.

| # | Sev | Area | One-liner |
|---|-----|------|-----------|
| F1 | High | Go binding | `OnTick`/`OnTaskChange` two-pass consumes + drops fired triggers; always returns empty |
| F2 | Medium | C header | consuming trigger calls documented like the idempotent two-pass readers |
| F3 | Medium | Go binding | `Snapshot()` two-pass truncates to a corrupt buffer if state grows mid-call |
| F4 | Low | C FFI | `TriggerEngineHandle`/`ShardGroupHandle` omit the sibling `HandleGuard` |
| F5 | Nit | FFI / Go | `claim` skips pre-zero of `out_island`; `ShardGroup` no public `Free()`; unguarded `free()` |
