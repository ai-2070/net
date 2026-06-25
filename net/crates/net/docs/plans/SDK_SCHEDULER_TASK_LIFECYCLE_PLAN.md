# Plan: Expose the Gang Scheduler + Task Lifecycle across the SDKs

**Status:** proposed (not started)
**Targets:** Rust SDK (`net-sdk`), TypeScript (`@net-mesh/sdk`), Python (`net_sdk`),
Go (`go/`), C (`include/net_*.h`)
**Depends on:** the gang-claim scheduler + cortex task-lifecycle landed on `master`
(`src/adapter/net/behavior/gang/*`, `src/adapter/net/behavior/fold/island.rs`,
`src/adapter/net/cortex/workflow/*`, node API in `src/adapter/net/mesh.rs`).

---

## 1. Summary

The gang-claim scheduler and the cortex task-lifecycle (`WorkflowAdapter`) exist
only in the `net` crate today — **zero** surface in `net-sdk` and zero in any of the
four language SDKs. This plan threads both features through every layer, mirroring
the patterns already established for `cortex` (Tasks/Memories) and the node/mesh
surfaces, so a TS, Python, Go, or C user can schedule GPU gangs and drive task
lifecycles without touching the core crate.

Two features, two natural homes (they ride different existing rails):

| Feature | Nature | Backed by | Mirrors existing | SDK home |
|---|---|---|---|---|
| **Task lifecycle** (`WorkflowAdapter`, shards, triggers) | local, event-sourced, async-open | a `Redex` log | `TasksAdapter` / `MemoriesAdapter` (cortex) | the **cortex** surface |
| **Gang scheduler** (publish/match/reserve/release/claim islands) | peer-aware, live mesh | a `MeshNode` (capability + island + reservation folds) | `announce_capabilities`, node RPC | the **mesh/node** surface |

Keeping the split is the whole ergonomic story: task lifecycle plugs into the
`Redex`/cortex builder users already know; gang ops hang off the live mesh handle
like capability announcements do.

---

## 2. Architecture recap (two binding mechanisms)

```
net crate (core)
  └─ net-sdk  (sdk/)               ── high-level Rust API: Mesh, cortex.rs, compute.rs …
       ├─ bindings/node  (#[napi]) → @net-mesh/core .node  → sdk-ts  (@net-mesh/sdk)
       ├─ bindings/python(#[pyclass])→ net (_net.so)        → sdk-py  (net_sdk)
       └─ bindings/go/*-ffi (C ABI cdylib) ── include/net_*.h ──┬→ go/ (cgo)
                                                                └→ pure C / C++ / Zig / …
```

- **TS + Python** wrap `net-sdk`/core types *directly* via napi-rs / PyO3 macros.
  Async is native (`AsyncTask<T>`→`Promise`; PyO3 `py.detach()` + `block_on`).
- **Go + C** consume hand-written `extern "C"` cdylibs (`bindings/go/*-ffi`) through
  the `include/net_*.h` headers. Async is bridged with a process-global
  `OnceLock<Arc<Runtime>>` + `block_on`, panics caught with an `ffi_guard!` macro,
  errors returned as status codes + thread-local last-error (or `**c_char` out-param).

Implication: **every public method must be authored up to four times** (Rust SDK →
napi → PyO3 → C-ABI), plus the language wrappers (sdk-ts, sdk-py, go, C example).
The plan front-loads the Rust SDK layer so the four bindings have one stable source.

---

## 3. API surface to expose (scoped into tiers)

The core crate surface is large; ship it in tiers so v1 is the high-value, stable
subset and the complex orchestration primitives land behind it.

### Tier 1 — core (v1)

**Gang (mesh/node surface):**
- `publish_island_topology(record) -> count` *(async)*
- `match_gpu_islands(criteria) -> [IslandId]` *(sync, read-only)*
- `reserve_island(island, until_unix_us) -> ClaimOutcome` *(async)*
- `release_island(island) -> ClaimOutcome` *(async)*
- `claim_gpu_island(criteria, until_unix_us) -> Option<IslandId>` *(async)*
- Types: `IslandRecord { id, gpus: GpuSet, host, warm_models, load, p50_latency_us }`,
  `MatchCriteria { capability, numeric: NumericFilter, selection: SelectionPolicy,
  prefer_warm_model }`, `NumericFilter`, `SelectionPolicy`, `ClaimOutcome {Won, Lost}`,
  `GpuSet`, `IslandId`, `ModelId`.

**Task lifecycle (cortex surface):**
- `WorkflowAdapter::open(redex, origin_hash)` / `open_with_config` *(async)*
- transitions: `submit`, `start`, `wait`, `block`, `complete`, `fail`, `transition`,
  `advance`, `retry`, `delete`, `link`, `request_cancel` → each returns a seq *(sync)*
- reads: `get(id) -> TaskState?`, `is_cancel_requested(id)`, `subtree(id) -> [TaskId]`,
  `status_counts() -> StatusCounts`
- durability: `snapshot()`, `open_from_snapshot()`, `wait_for_seq(seq)` *(async)*
- Types: `TaskId`, `TaskStatus {Submitted,Running,Waiting,Blocked,Done,Failed}`,
  `TaskState { step, status, attempts }`, `StatusCounts`.

### Tier 2 — orchestration helpers

- **Shards (fan-out / fan-in):** `ShardGroup`, `derive_shard_ids`, `fan_out`,
  `try_join` / `try_join_with(JoinPolicy)`, `JoinStatus`, `Join`, `propagate_failure`,
  `block_on_failure`. Pure-ish over a `WorkflowAdapter`; straightforward to wrap.
- **Triggers:** `TriggerEngine` (`arm`, `on_task_change`, `on_tick`, `on_delete`,
  `armed_count`), `Trigger {AfterTask, AfterTerminal, IfResult, AtTick}`,
  `Action {Submit, Start}`, `TriggerWorld`. Stateful in-memory; the cross-FFI shape
  needs an explicit decision (Tier-2 design note below).

### Tier 3 — advanced / deferred

- The capability-step seam: `drive_capability_step`, `GangClaimPipeline`,
  `CapabilityRequirement`, `ClaimResult`, `StepGate`, `ActiveClaim`, `release_step`
  (couples both features; the durable epoch/cohort wiring is still Phase-D work — see
  `MESH_SCHEDULER_GANG_CLAIM_PLAN.md` review #4).
- Low-level gang primitives: `commit_active`, `acquire_gang` / `try_acquire_gang`,
  `GangScheduler` / `schedule_gang`, `Claimant`, quorum/fence types. Power-user only.

> **Decision needed (D1):** confirm Tier 1 is the v1 cut. Recommendation: ship Tier 1
> across all four SDKs first; Tier 2 (shards + triggers) as a fast follow; Tier 3 stays
> Rust-only until there's a concrete cross-language consumer.

---

## 4. Layer 0 — Rust SDK (`net-sdk`) — the single source the bindings wrap

The bindings (napi, PyO3, and the -ffi crates) should wrap **net-sdk** types, not reach
into the core crate, so there is one ergonomic surface to keep in parity.

### 4a. Task lifecycle → extend `sdk/src/cortex.rs`

`cortex.rs` already re-exports `TasksAdapter` / `MemoriesAdapter` / `NetDb`. Add the
workflow surface alongside:

- Re-export `WorkflowAdapter`, `TaskId`, `TaskState`, `TaskStatus`, `StatusCounts`,
  and `CortexAdapterError` (already surfaced).
- Decide whether to fold `WorkflowAdapter` into the `NetDb` builder
  (`.with_workflow()`, parallel to `.with_tasks()`) or expose it standalone like the
  raw `TasksAdapter::open`. Recommendation: **both** — standalone open for the simple
  case, `NetDb` integration for the bundled case (matches Tasks/Memories).
- Shards + triggers (Tier 2) live here too (they operate over a `WorkflowAdapter`).

### 4b. Gang → new `sdk/src/gang.rs` + methods on `Mesh`

- New module `sdk/src/gang.rs` re-exporting the gang/island types
  (`MatchCriteria`, `NumericFilter`, `SelectionPolicy`, `ClaimOutcome`, `IslandRecord`,
  `GpuSet`, `IslandId`, `ModelId`, `ClaimError`).
- Add the five node methods to `impl Mesh` in `sdk/src/mesh.rs`, delegating to the
  inner `Arc<MeshNode>` and mapping errors to `SdkError` — exactly how
  `announce_capabilities` already bridges a peer-aware node call. Pattern:
  ```rust
  pub async fn claim_gpu_island(&self, c: &gang::MatchCriteria, until_us: u64)
      -> Result<Option<gang::IslandId>> {
      self.node_arc().claim_gpu_island(c, until_us).await
          .map_err(|e| SdkError::Adapter(e.to_string()))
  }
  ```
- Declare `pub mod gang;` in `sdk/src/lib.rs` (feature-gated — see D2).

> **Decision needed (D2):** feature gating. Task lifecycle rides the existing `cortex`
> feature. Gang is GPU-scheduling — gate it under `compute`, or add a dedicated
> `scheduler` feature? Recommendation: a `scheduler` feature that implies the gang +
> workflow surface, so a consumer can pull the scheduler without the full compute
> runtime; default it on where `compute` is on.

### 4c. Error mapping

`SdkError` is `#[non_exhaustive]`; add variants if a typed error is wanted
(`SdkError::Claim`, `SdkError::Workflow`) or reuse `SdkError::Adapter(String)` for v1.
Keep the core `ClaimOutcome::Lost` as a *value* (not an error) — losing a claim is a
normal outcome, only `ClaimError`/`CortexAdapterError` are errors.

---

## 5. Layer 1a — TypeScript (`bindings/node` + `sdk-ts`)

**Native (`bindings/node/src/`):** add `gang.rs` and extend `cortex.rs` with `#[napi]`
classes wrapping the net-sdk types. Follow the `TasksAdapter` exemplar:
- `#[napi]` struct holding `Arc<Inner…>`; `#[napi(factory)] open(...) -> AsyncTask<Self>`
  for async constructors; sync methods return `Result<BigInt>` (u64 → `BigInt`).
- Gang methods attach to the existing `NetMesh` napi class (peer-aware), task-lifecycle
  is a new `WorkflowAdapter` napi class (Redex-backed).
- Register in `bindings/node/src/lib.rs`; gate via the binding's Cargo features.

**Wrapper (`sdk-ts/src/`):** add `gang.ts` and extend `cortex.ts` with the ergonomic
classes that hold the `Napi*` instance and `classifyError`-wrap every call (mirror
`cortex.ts`'s `TasksAdapter`). Watch/stream surfaces (if any in Tier 2) use the
existing `wrapWatchIter` → `AsyncIterable` helper. Export from `sdk-ts/src/index.ts`.

**Types:** hand-mirror Rust structs/enums as TS `type`s; `u64`→`bigint`, enums→string
literal unions (`"won" | "lost"`, the `TaskStatus` set). Tests in `sdk-ts/test/`.

---

## 6. Layer 1b — Python (`bindings/python` + `sdk-py`)

**Native (`bindings/python/src/`):** add `gang.rs` and extend `cortex.rs` with
`#[pyclass]`/`#[pymethods]`. Follow the `PyTasksAdapter` exemplar:
- `#[staticmethod] open(...)` runs `runtime.block_on` under `py.detach()` (off-GIL);
  sync transition methods return `PyResult<u64>`.
- `From<Inner…>` impls to convert core types → `#[pyclass]` views (`PyTaskState`,
  `PyClaimOutcome`, `PyIslandRecord`); fields via `#[pyo3(get)]`.
- Gang methods attach to the existing mesh `#[pyclass]`; create a `GangError` /
  reuse `CortexError` exception. Register in `bindings/python/src/lib.rs`.

**Wrapper (`sdk-py/src/net_sdk/`):** add `gang.py` and extend `cortex.py` with the
`try: from net import …` guarded imports + `@contextmanager` helpers
(`workflow_cm`, `gang_…`), mirroring `tasks_cm`. Export from `__init__.py`. Type hints
as `Literal[...]` for enums. Tests in `sdk-py/tests/`.

---

## 7. Layer 1c — Go + C (`bindings/go/scheduler-ffi` + `include/` + `go/`)

This is the heavier layer — a hand-written C ABI plus two consumers (Go cgo + pure C).

**New FFI crate `bindings/go/scheduler-ffi/`** (`cdylib`+`staticlib`+`rlib`, lib name
`net_scheduler`, added to the workspace `members`). Implements the established FFI
contract:
- Opaque handles via `Box::into_raw`/`from_raw`; `*_new` / `*_free` (+ `*_shutdown`
  for the async-holding handles).
- Process-global `OnceLock<Arc<Runtime>>` + `block_on` (with the runtime-in-runtime
  abort guard); every entry point wrapped in `ffi_guard!` (`catch_unwind`).
- Status codes (`NET_SCHEDULER_OK=0`, negatives) + thread-local last-error pair
  (`net_scheduler_last_error_message/_kind/_clear`) — matches `meshos-ffi`.
- Marshaling: `#[repr(C)]` for `IslandRecord`/`TaskState`/`StatusCounts` wire structs;
  `(ptr,len)` for the GPU set / warm-model arrays and the island-id result list;
  `ClaimOutcome`/`TaskStatus`/`SelectionPolicy` as `c_int` discriminants.

> **Decision needed (D3):** the gang surface needs a *node* handle. Reuse the existing
> node/mesh handle the other -ffi crates accept (e.g. the `nodeArcPtr` pattern
> `net_rpc_new` uses) rather than minting a new node ctor in scheduler-ffi. The
> task-lifecycle surface needs a `Redex` handle — reuse the cortex C ABI's Redex
> handle (`net_cortex.h`) rather than re-opening one. Confirm both handles are
> exportable/acceptable across crate boundaries.

**C header(s) in `include/`:** add `net_scheduler.h` (hand-written, documented like
`net_rpc.h`/`net_meshos.h`): build/link notes, ABI version, status codes, opaque
typedefs, `repr(C)` structs, and the function signatures for both gang + workflow.
*(Alternative: extend `net.h` for gang and `net_cortex.h` for workflow instead of a
new header — see D4.)*

> **Decision needed (D4):** one new `net_scheduler.h`, or extend `net.h` (gang, since
> it's node-level) + `net_cortex.h` (workflow, since it's Redex-level)? Recommendation:
> extend the existing two — gang is a node capability and workflow is a cortex adapter,
> so they belong with their siblings; a standalone header fragments the node/cortex
> stories.

**Go wrapper:** add `scheduler.go` (or `gang.go` + `workflow.go`) in
`bindings/go/net/` (the reference contract) **and** vendor the same into the root
`go/` module. Follow the `mesh_rpc.go` exemplar: struct with `*C.Handle` +
`sync.RWMutex` + `runtime.SetFinalizer`; `withHandle`; `parse…Error` → typed Go
error with a `Kind` discriminator; `[]byte`/array crossing via `unsafe.Pointer` +
`C.memmove`; `#cgo LDFLAGS: -lnet_scheduler`.

**C example:** add `examples/scheduler.c` (mirrors `examples/meshos.c`) showing a
publish→match→claim→release loop and a submit→start→complete task walkthrough — this
doubles as the C "SDK" smoke test and documentation.

---

## 8. Phasing (ordered work plan)

Each phase ends green (builds + tests) and is independently reviewable.

- **P0 — Rust SDK surface (Tier 1).** `sdk/src/gang.rs`, `Mesh` methods,
  `cortex.rs` workflow re-exports, feature gating (D2), SDK tests
  (`sdk/tests/gang_surface.rs`, `sdk/tests/workflow_surface.rs`). *Unblocks everyone.*
- **P1 — TypeScript (Tier 1).** napi `gang.rs`/`cortex.rs` additions → `@net-mesh/core`;
  `sdk-ts` wrappers + types + vitest. *(napi/PyO3 wrap net-sdk, so P1/P2 can parallelize
  after P0.)*
- **P2 — Python (Tier 1).** PyO3 additions → `net_sdk` wrappers + pytest.
- **P3 — C ABI + Go + C (Tier 1).** `scheduler-ffi` crate, header(s) (D3/D4), Go
  reference + vendored wrappers, `examples/scheduler.c`, Go tests. *(Heaviest; can run
  in parallel with P1/P2 but shares the D3/D4 decisions.)*
- **P4 — Tier 2 (shards + triggers)** across all four SDKs, same layer order.
  Triggers need the cross-FFI design note resolved first (below).
- **P5 — docs + parity sweep.** Per-SDK README sections, the parity matrix kept honest,
  a cross-language smoke test in CI.

**Tier-2 trigger design note:** `TriggerEngine` is a stateful in-memory struct whose
`on_task_change`/`on_tick` return `Action`s the *caller* applies. For napi/PyO3 it maps
directly (a class returning an array of actions). For the C ABI, return the actions as a
`repr(C)` array the caller drains (avoid callbacks if possible) — the engine performs no
I/O, so a pure "evaluate → return actions" call is the cleanest boundary.

---

## 9. Parity matrix (keep honest as phases land)

| Capability | Rust SDK | TS | Python | Go | C |
|---|---|---|---|---|---|
| publish/match/reserve/release/claim island | ✅ | ✅ | ✅ | ☐ | ☐ |
| gang/island types | ✅ | ✅ | ✅ | ☐ | ☐ |
| WorkflowAdapter open + transitions | ✅ | ✅ | ✅ | ☐ | ☐ |
| Workflow reads (get/status_counts/subtree) | ✅ | ✅ | ✅ | ☐ | ☐ |
| Workflow snapshot/restore | ✅ | ✅ | ✅ | ☐ | ☐ |
| Shards (fan_out/try_join/propagate) — T2 | ☐ | ☐ | ☐ | ☐ | ☐ |
| Triggers — T2 | ☐ | ☐ | ☐ | ☐ | ☐ |

**Progress:** P0 (Rust SDK), P1 (TypeScript), P2 (Python) landed Tier 1 — all
verified end-to-end (Rust surface tests; napi build + tsc + vitest; maturin build +
pytest). P3 (Go/C) next. Python gang takes flat kwargs (no nested-criteria object),
same as the napi side.

---

## 10. Testing strategy

- **Rust SDK:** integration tests opening a real `Redex` (workflow) and a 1–2 node
  `Mesh` (gang) — reuse the `tests/gang_claim_node.rs` fixtures as a template.
- **TS / Python:** per-binding tests against the built native artifact (vitest / pytest),
  asserting the same lifecycle a Rust test does (submit→start→complete; publish→claim→release).
- **Go:** `*_test.go` against the cdylib (the `mesh_rpc` tests are the template);
  exercise handle free + finalizer + error-kind parsing.
- **C:** `examples/scheduler.c` compiled and run in CI as a smoke test (like the meshos
  example), proving the header + cdylib link and a happy-path round-trip.
- **Cross-language parity:** one scripted scenario (publish an island in language A,
  claim it in language B over a real mesh) to catch ABI/serialization drift. Optional
  but high-value for the gang path.
- **CI:** the `fmt`, `loom-tests`, and per-binding jobs already exist; add the
  scheduler-ffi crate to the workspace build and the C example to the C-smoke job.

---

## 11. Open questions / decisions

- **D1** — Confirm Tier 1 as the v1 cut (gang node API + WorkflowAdapter core);
  shards/triggers as Tier 2; pipeline/low-level primitives Tier 3.
- **D2** — Feature gating: dedicated `scheduler` feature vs ride `compute`/`cortex`.
- **D3** — Handle reuse in the C ABI: accept the existing node handle (gang) and Redex
  handle (workflow) rather than minting new constructors in `scheduler-ffi`.
- **D4** — One `net_scheduler.h` vs extend `net.h` (gang) + `net_cortex.h` (workflow).
- **D5** — Triggers cross-FFI shape (return-actions array vs callbacks) — Tier 2.
- **D6** — `IslandId`/`TaskId` are `u64`; confirm `BigInt` (TS) / `int` (Py) / `uint64_t`
  (Go/C) precision handling is consistent (TS already uses `BigInt` for u64 elsewhere).
- **D7** — Gang `claim_gpu_island` depends on a live, connected mesh; document the
  prerequisite (peers + capability/island folds populated) so SDK users don't expect it
  to work on an isolated node (recall review #1: a node now sees its *own* published
  islands, but cross-node matches need convergence).

---

## 12. Non-goals

- No changes to the core gang/workflow semantics — this is pure surfacing.
- No durable-fence / shared-cohort Phase-D wiring (tracked in
  `MESH_SCHEDULER_GANG_CLAIM_PLAN.md` review #4); the SDK exposes what exists.
- No new orchestration logic in the SDKs — they re-export/wrap, they don't add
  business logic (the established cortex/capability SDK convention).
