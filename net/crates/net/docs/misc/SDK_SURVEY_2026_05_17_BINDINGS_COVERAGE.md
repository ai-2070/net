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
