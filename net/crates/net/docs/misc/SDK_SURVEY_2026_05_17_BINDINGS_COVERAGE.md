# SDK Binding Coverage Survey — 2026-05-17

Surveys the per-language SDK surface for the four persistence/folded-state
layers (**RedEX**, **RedEX disk**, **CortEX**, **NetDb**) and the two
substrate query/daemon layers (**MeshDB**, **MeshOS**).

Verified against source in `net/crates/net/` rather than docs.

---

## 1. RedEX / RedEX-disk / CortEX / NetDb

| Surface | Node | Python | Go | C |
|---|---|---|---|---|
| **RedEX** (in-memory log) | `Redex` — napi `bindings/node/index.d.ts:1270` + wrapper `sdk-ts/src/cortex.ts:128` | `net.Redex` (PyO3) — `bindings/python/python/net/_net.pyi:249` | `Redex` / `NewRedex()` — `bindings/go/net/redex.go:328` and top-level `go/cortex.go:114` | `net_redex_new()` — `include/net.go.h:260` (not in `include/net.h`) |
| **RedEX disk** (persistent) | `new Redex({ persistentDir })` | `Redex(persistent_dir=...)` | `NewRedexWithPersistentDir(dir)` — `bindings/go/net/redex.go:337` | `net_redex_new_with_persistent_dir(path)` (via `net.go.h`) |
| **CortEX** (Tasks + Memories adapters, watchers) | `TasksAdapter`, `MemoriesAdapter` — `sdk-ts/src/cortex.ts:386,550` | `TasksAdapter`, `MemoriesAdapter`, `Task`, `Memory`, `*WatchIter` — `_net.pyi:340,426` | `OpenTasksAdapter` / `OpenMemoriesAdapter` — `bindings/go/net/tasks.go:235`, `memories.go:200`; plus `OpenTasks` / `OpenMemories` aliases in top-level `go/cortex.go` | `net_tasks_*`, `net_memories_*` — `include/net.go.h:283-339` |
| **NetDb** (cross-adapter façade + snapshot bundle) | `NetDb.open()` / `openFromSnapshot()` / `snapshot()` — `index.d.ts:1270`, sdk-ts `cortex.ts:708` | `net.NetDb` — `_net.pyi:512` | **Not exposed.** Rust FFI source notes "Go-side `NetDb` struct composes" (`src/ffi/cortex.rs:1441`); only `ErrNetDb` is reserved for forward compatibility | **No `net_netdb_*` symbols** in any shipped header. `NetDb` composition only happens inside the Node/Python binding crates (which call Rust directly, not the cortex FFI) |

### Notes

- **Canonical C `net.h` is bus-only.** The RedEX/CortEX C surface lives in
  `include/net.go.h` (the 320-line `net.h` has no `redex`/`cortex`/`tasks`/
  `memories` symbols). The "Go" filename is a misnomer — it's a plain C
  header any C / C++ / Zig / Swift consumer can link against. The dedicated
  `net_meshdb.h` next to it is a *different* product (MeshDB query layer),
  not NetDb.
- **Python split.** The cortex/NetDb surface is **only** in the low-level
  PyO3 binding (`bindings/python` → `net._net`). The higher-level `sdk-py`
  package (`net_sdk`) has no `redex.py` / `cortex.py` / `netdb.py` and
  does not re-export them. So `from net import Redex, NetDb`, not
  `from net_sdk import …`.
- **Feature gate.** The whole stack is behind the Rust `cortex` Cargo
  feature. Python's `__init__.py` try-imports it
  (`bindings/python/python/net/__init__.py:67-107`); wheels built without
  `cortex` silently omit the symbols.
- **NetDb gap on Go/C is structural, not a doc bug.** The `cortex.rs`
  FFI deliberately ships adapters individually; NetDb composition is done
  in the napi-rs / PyO3 layer by calling the Rust core directly. To get
  equivalent semantics from Go or C today, callers open `Tasks` +
  `Memories` separately and query each; there's no single
  snapshot/restore bundle.

---

## 2. MeshDB / MeshOS

| Surface | Node | Python | Go | C |
|---|---|---|---|---|
| **MeshDB** (query layer: AST + runner + chain reader) | napi `bindings/node` — `MeshQuery`, `MeshQueryRunner`, `MeshQueryStream` (`index.d.ts:883,975,998`) + helper `meshdb.ts`. **Not in `sdk-ts`** — no `meshdb.ts` wrapper | PyO3 `net._net` (via `__init__.py:516-553`): `MeshQuery`, `MeshQueryRunner`, `InMemoryChainReader`, `QueryBuilder`, `Predicate`, `ResultRow`, `AggregateResult`, `JoinedRow`, `WindowBoundary`, `GroupKey`, `LineageEntry`, `CachePolicy`, `ExecuteOptions`, `MeshDbError`. Typed stub `_net.pyi` does not declare them — runtime-only. No `net_sdk` wrapper module | `bindings/go/net/meshdb.go` — `MeshDBReader`, `MeshDBResult`, `MeshDBResultRow`, `MeshDBError`, `NewMeshDBReader()`. **Not re-exported from top-level `go/`** | Dedicated `include/net_meshdb.h` + `libnet_meshdb` cdylib (9 prototypes — small surface: Reader / Runner / Query / Iter). Built from `bindings/go/meshdb-ffi` |
| **MeshOS** (daemon-author SDK) | napi `bindings/node` — `MeshOsDaemonHandle`, `MeshOsDaemonSdk` (`index.d.ts:773,833`) + `MeshOsConfigJs`. Plus `sdk-ts/src/meshos.ts:263,338` — `MeshOsDaemonSdk`, `MeshOsDaemonHandle`, `MeshOsSdkError` ergonomic wrapper | PyO3 `net._net` — `MeshOsDaemonSdk`, `MeshOsDaemonHandle`, `MeshOsSdkError` (`__init__.py:356-401`). Plus high-level `sdk-py/src/net_sdk/meshos.py` — `MeshOsDaemon` Protocol, typed-dict envelopes (`DaemonControl`, `MaintenanceState`), context-manager wrappers. Typed stub `_net.pyi` does not declare them — runtime-only | `bindings/go/net/meshos.go` — full surface (~1.2k LOC): `MeshOsDaemonSdk`, `MeshOsDaemonHandle`, `MeshOsConfig`, `MeshOsDaemon` interface with Go-side callback trampolines (`Snapshot` / `Restore` / `OnControl` / `Health` / `Saturation`), `MeshOsMetadataView`, `PublishCapabilities`, context-aware variants. Not re-exported from top-level `go/` | Dedicated `include/net_meshos.h` + `libnet_meshos` cdylib (~46 prototypes — full surface). Built from `bindings/go/meshos-ffi` |

### Notes

- **Both are separate cdylibs.** Unlike RedEX/CortEX (which the Go binding
  pulls in via `net.go.h`), MeshDB and MeshOS each ship their own header +
  shared library — `libnet_meshdb.{so,dylib,dll}` and
  `libnet_meshos.{so,dylib,dll}`. `net.go.h` contains zero `meshdb` /
  `meshos` symbols. C consumers link the lib they want.
- **TS coverage is asymmetric.** `sdk-ts` wraps MeshOS but **not MeshDB** —
  Node MeshDB callers go directly to the napi binding (`@ai2070/net` /
  `bindings/node`). MeshOS callers get both a low-level napi class and the
  sdk-ts ergonomic wrapper.
- **Python coverage is asymmetric in the opposite direction.** `sdk-py`
  wraps **MeshOS** (`net_sdk/meshos.py` adds Protocol + typed-dicts +
  context managers) but **not MeshDB**. MeshDB consumers import directly
  from the `net` package (`from net import MeshQuery, MeshQueryRunner,
  InMemoryChainReader, …`).
- **Python typed stub `_net.pyi` is stale for both.** The stub stops at
  the cortex surface (around line 843, `class Identity`). MeshDB and
  MeshOS classes are imported and re-exported at runtime in
  `__init__.py`, but type checkers and IDE autocomplete won't see them
  unless the stub is extended.
- **Go has both at the binding level only.** The top-level `go/` package
  (the "high-level Go SDK") exposes RedEX / CortEX wrappers (`Redex`,
  `OpenTasks`, etc.) but does **not** re-export MeshDB or MeshOS — Go
  consumers `import "github.com/ai-2070/net/bindings/go/net"` directly
  for those.
- **MeshOS callback model varies.** Go uses cgo trampolines
  (`goMeshOsProcessTrampoline`, `goMeshOsSnapshotTrampoline`, …) to
  surface a Go `MeshOsDaemon` interface back through the FFI; Python
  uses a Protocol class; Node currently exposes `RegisterDaemon` without
  a callback-trait surface (verify napi exposes a daemon callback trait
  before assuming parity with Go/Python).
- **Cargo feature gates.** MeshDB is behind the `meshdb` feature; MeshOS
  is behind `meshos`. Wheels / dylibs built without them silently omit
  the symbols (Python's `__init__.py` try-imports both feature blocks).

---

## Cross-cutting observations

1. **Two Python entry points, one canonical.** `net._net` (PyO3, runtime
   ground truth) vs. `net_sdk` (pure-Python ergonomic wrappers, partial
   coverage). RedEX / CortEX / NetDb / MeshDB are PyO3-only today;
   MeshOS is the one surface that has a sdk-py wrapper.
2. **Two Node entry points, one canonical.** `@ai2070/net`
   (napi-rs `bindings/node`, ground truth) vs. `sdk-ts` (ergonomic
   wrappers). MeshDB is napi-only on the Node side; all other surfaces
   have sdk-ts wrappers.
3. **C ABI is split across three+ headers.** `net.h` (bus only),
   `net.go.h` (bus + RedEX + CortEX + RPC + Deck + … — the catch-all
   "Go" header), `net_meshdb.h`, `net_meshos.h`. Pick the right one;
   `net.h` alone gets you almost nothing beyond ingest/poll.
4. **NetDb gap on Go/C and `sdk-py` wrapper gaps for RedEX/CortEX/NetDb/
   MeshDB** are the largest parity gaps. If parity matters for any of
   these surfaces, those are the targets.

---

## Plan to fix

Grouped by impact. Items within a tier are roughly independent and can be
parallelized; cross-tier ordering matters (Tier 1 enables a couple of
Tier 2 items).

### Tier 1 — capability gaps (the binding can't do something a peer binding can)

**P1.1 — Land `NetDb` FFI surface.** The Rust core already has
`NetDb` / `NetDbBuilder` / `NetDbSnapshot`; the cortex FFI in
`src/ffi/cortex.rs` reserves `NET_ERR_NETDB` and even comments
"Go-side `NetDb` struct composes" but ships no symbols. Add:

  - `net_netdb_open(redex, config_json, *out_handle) -> int`
  - `net_netdb_open_from_snapshot(redex, config_json, bundle_bytes, bundle_len, *out_handle) -> int`
  - `net_netdb_snapshot(handle, *out_bytes, *out_len) -> int`
  - `net_netdb_tasks(handle, *out_tasks_handle) -> int` (borrow, no-free)
  - `net_netdb_memories(handle, *out_memories_handle) -> int` (borrow, no-free)
  - `net_netdb_close(handle) -> int` / `net_netdb_free(handle)`

  Borrow semantics on the adapter accessors avoid double-free with
  `net_tasks_adapter_free` — document that the returned handle's
  lifetime is bounded by the parent NetDb. Mirror the existing handle
  convention from RedEX (opaque `*mut T`, JSON config, idempotent
  free).

  Files: `src/ffi/cortex.rs`, `include/net.go.h` (extend with the new
  prototypes), regression test that scans both headers for drift
  (existing scaffold per `net.h` comment at line 38).

**P1.2 — Expose `NetDb` in the Go binding.** Once P1.1 lands, add
`bindings/go/net/netdb.go` with `NetDb` type, `OpenNetDb(redex,
config)` / `OpenNetDbFromSnapshot(redex, config, bundle)`,
`(*NetDb).Tasks() *TasksAdapter`, `(*NetDb).Memories() *MemoriesAdapter`,
`(*NetDb).Snapshot() ([]byte, error)`, `(*NetDb).Close() error`. Match
the Go binding's existing finalizer + read-lock pattern (see
`redex.go` for the template). Re-export from top-level `go/` (see P2.4).

  Files: `bindings/go/net/netdb.go` (new), `bindings/go/net/netdb_test.go`
  (new), `bindings/go/meshdb-ffi/Cargo.toml` review (if NetDb FFI builds
  as part of a different ffi crate, wire the new cdylib symbols here).

**P1.3 — Node MeshOS daemon callback trait.** Node's
`MeshOsDaemonSdk.registerDaemon` accepts a name + seed but no
`process` / `snapshot` / `restore` / `onControl` / `health` /
`saturation` callbacks. Go and Python both surface this (Go via cgo
trampolines `goMeshOsProcessTrampoline` etc.; Python via Protocol +
`register_daemon_with_callbacks`). Add a napi entry point that takes
TSFNs for each trait method and bridges them to the same C trampoline
pattern the `DaemonRuntime.spawn` napi already uses for compute
daemons. The pattern is proven — `index.d.ts:230` shows `spawn(...,
process, snapshot?, restore?, ...)` already works for compute.

  Files: `bindings/node/src/meshos.rs` (or wherever the napi MeshOS
  impl lives — confirm with the napi crate layout), `bindings/node/index.d.ts`
  signature update, `bindings/node/test/meshos.test.ts` test
  covering at least process + snapshot + restore round-trip.

### Tier 2 — DX / ergonomic parity (capability exists but is rough)

**P2.1 — `sdk-py` wrapper modules for RedEX / CortEX / NetDb / MeshDB.**
Mirror the `meshos.py` pattern: re-import from `net._net`, add typed
shapes (`TypedDict` envelopes, `Protocol` classes where there's a
callback trait, context-manager wrappers around open/close). Four new
modules:

  - `net_sdk/redex.py` — `Redex`, `RedexFile`, `RedexEvent`,
    `RedexTailIter` re-exports + a context-manager helper
    `open_redex_file(path, **cfg)` that yields a `RedexFile` and
    closes on exit.
  - `net_sdk/cortex.py` — `TasksAdapter`, `MemoriesAdapter`, `Task`,
    `Memory`, watch iterators + a `Task` / `Memory` `TypedDict`
    matching the `find_many` result shape.
  - `net_sdk/netdb.py` — `NetDb` re-export + `open_netdb(redex,
    *adapters)` builder helper.
  - `net_sdk/meshdb.py` — `MeshQuery`, `MeshQueryRunner`,
    `InMemoryChainReader`, `Predicate`, `QueryBuilder` re-exports +
    a `MeshQueryRunner` context manager.

  Mirror the `meshos.py` docstring + `from __future__ import
  annotations` shape so the modules are consistent.

  Files: four new files under `sdk-py/src/net_sdk/`, plus
  `sdk-py/src/net_sdk/__init__.py` if there's a re-export aggregator.

**P2.2 — `sdk-ts` wrapper for MeshDB.** `sdk-ts` has wrappers for
RedEX / CortEX / MeshOS / mesh / nRPC but not MeshDB. Add
`sdk-ts/src/meshdb.ts` re-exporting `MeshQuery`, `MeshQueryRunner`,
`MeshQueryStream`, `InMemoryChainReader` from `@ai2070/net` and
adding a `using`-friendly disposable wrapper (Symbol.dispose) so
runners can be used with TC39 explicit resource management.

  Files: `sdk-ts/src/meshdb.ts` (new), `sdk-ts/src/index.ts` re-export
  update, `sdk-ts/test/meshdb.test.ts` (new).

**P2.3 — Extend `_net.pyi` type stub.** Stub stops at line 843
(`class Identity`); MeshDb, MeshQuery, MeshQueryRunner,
InMemoryChainReader, MeshOsDaemonSdk, MeshOsDaemonHandle, Predicate,
QueryBuilder, ResultRow, AggregateResult, JoinedRow, WindowBoundary,
GroupKey, LineageEntry, CachePolicy, ExecuteOptions, MeshDbError,
MeshOsSdkError, BlobError, BlobRef, MeshBlobAdapter, DaemonRuntime,
DaemonHandle, ForkGroup, ReplicaGroup, StandbyGroup, GroupError,
DeckClient, OperatorIdentity, DeckSdkError — none are typed. Extend
the stub to cover everything `__init__.py` re-exports.

  Files: `bindings/python/python/net/_net.pyi` (append class
  declarations matching the runtime surface). Per-class signature
  source: the PyO3 `#[pymethods]` blocks in the matching Rust crate
  (`bindings/python/src/meshdb.rs`, `meshos.rs`, etc.).

**P2.4 — Top-level `go/` re-exports for MeshDB and MeshOS (and NetDb
once P1.2 lands).** Top-level `go/` already wraps RedEX / CortEX via
`go/cortex.go`. Add `go/meshdb.go` and `go/meshos.go` re-exporting
the `bindings/go/net` types. Two options:

  (a) Type aliases (`type MeshDBReader = netbindings.MeshDBReader`)
      — zero overhead, propagates everything.
  (b) Thin wrapper types — gives a stable top-level API even if the
      binding moves under it.

  Pick (a) for now (lower maintenance); switch to (b) if the binding
  ever needs to evolve independently. Same call for NetDb once P1.2
  is in.

  Files: `go/meshdb.go` (new), `go/meshos.go` (new), `go/netdb.go`
  (new, depends on P1.2).

### Tier 3 — structural / documentation cleanup

**P3.1 — Split RedEX / CortEX out of `net.go.h` into a dedicated
`net_cortex.h`.** The "Go" header name is misleading — it's a valid
C header for any consumer, and burying RedEX / CortEX / Tasks /
Memories inside a file named `net.go.h` makes the C SDK story
opaque. Mirror the pattern of `net_meshdb.h` / `net_meshos.h`:

  - Move `net_redex_*`, `net_tasks_*`, `net_memories_*`, and (once
    P1.1 lands) `net_netdb_*` prototypes from `net.go.h` into a new
    `include/net_cortex.h`.
  - Keep `net.go.h` as a convenience header that `#include`s the
    submodule headers for callers who want everything in one place
    (the Go cgo blocks already declare prototypes inline, so they're
    unaffected).
  - Update `include/README.md` to document the four-header layout
    (`net.h` for bus, `net_cortex.h` for RedEX/CortEX/NetDb,
    `net_meshdb.h` for MeshDB, `net_meshos.h` for MeshOS,
    `net_rpc.h` / `net_deck.h` for their respective surfaces).

  Files: `include/net_cortex.h` (new), `include/net.go.h` (trim +
  re-include), `include/README.md`.

**P3.2 — Cargo feature documentation.** Document the
`cortex` / `meshdb` / `meshos` Cargo feature gates in the binding
READMEs so consumers know what to ask for when building wheels /
dylibs / npm packages. Today the gating only shows up in
`bindings/python/python/net/__init__.py` try-imports — a wheel built
without `cortex` silently lacks `Redex` but reports no build-time
hint.

  Files: `bindings/python/README.md`, `bindings/node/README.md`,
  `bindings/go/net/README.md` (if any), `sdk-py/README.md`,
  `sdk-ts/README.md`. Add a "Cargo features" table to each.

### Dependency graph

```
P1.1 (NetDb FFI)
  └── P1.2 (Go NetDb)
        └── P2.4 (top-level go/ NetDb re-export)
  └── (Node/Python already use Rust core directly, unaffected)

P1.3 (Node MeshOS callbacks)   — independent

P2.1 (sdk-py wrappers)          — independent (each module independent)
P2.2 (sdk-ts MeshDB)            — independent
P2.3 (_net.pyi stub)            — independent

P2.4 (go/ re-exports)
  └── partially blocked on P1.2 for the NetDb piece; MeshDB / MeshOS
      pieces are ready today.

P3.1 (split net.go.h)
  └── best to land after P1.1 so the new `net_cortex.h` is born
      with NetDb prototypes rather than acquiring them in a follow-up.

P3.2 (feature docs)             — independent, do last.
```

### Suggested execution order

1. **P1.1** (NetDb FFI) — unblocks Go + the C `net_cortex.h` split.
2. **P1.3** (Node MeshOS callbacks) — true capability gap, no
   dependencies. Can run parallel to P1.1.
3. **P1.2** (Go NetDb binding) + **P2.3** (`_net.pyi` stub) +
   **P2.2** (sdk-ts MeshDB) — parallelizable after P1.1.
4. **P2.1** (sdk-py wrappers) — pure-Python work, can start any
   time but easier to validate once P2.3's stub is in.
5. **P2.4** (top-level Go re-exports) — small, mechanical, do after
   P1.2.
6. **P3.1** (header split) — touches the C ABI surface; sequence
   after P1.1 lands so it covers NetDb in the first pass.
7. **P3.2** (feature-flag docs) — close out.

### Rough effort estimate

| Item | Effort |
|---|---|
| P1.1 NetDb FFI | M (mirror RedEX FFI pattern; ~200-300 LOC + tests) |
| P1.2 Go NetDb | S (mirror redex.go pattern) |
| P1.3 Node MeshOS callbacks | M (mirror compute DaemonRuntime napi TSFN pattern) |
| P2.1 sdk-py wrappers (4 modules) | S each, M total |
| P2.2 sdk-ts MeshDB | S |
| P2.3 `_net.pyi` stub | M (~20+ classes, mechanical) |
| P2.4 go/ re-exports | XS (3 alias files) |
| P3.1 header split | S (mechanical move + README) |
| P3.2 feature docs | S |

XS = under an hour, S = half-day, M = 1-2 days, L = a week.
