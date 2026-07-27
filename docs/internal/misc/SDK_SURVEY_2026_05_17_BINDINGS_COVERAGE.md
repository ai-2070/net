# SDK Binding Coverage Survey — 2026-05-17

Surveys the per-language SDK surface for the four persistence/folded-state
layers (**RedEX**, **RedEX disk**, **CortEX**, **NetDb**), the two
substrate query/daemon layers (**MeshDB**, **MeshOS**), and the operator
control plane (**Deck**).

Verified against source in `net/crates/net/` rather than docs. Re-verified
after commit `33edd522` ("Split RedEX / CortEX / NetDb out of net.go.h into
net_cortex.h") which landed P1.1 (NetDb C FFI) and P3.1 (header split) from
the original plan.

---

## 1. RedEX / RedEX-disk / CortEX / NetDb

| Surface | Node | Python | Go | C |
|---|---|---|---|---|
| **RedEX** (in-memory log) | `Redex` — napi `bindings/node/index.d.ts:1270` + wrapper `sdk-ts/src/cortex.ts:128` | `net.Redex` (PyO3) — `bindings/python/python/net/_net.pyi:249` | `Redex` / `NewRedex()` — `bindings/go/net/redex.go:328` and top-level `go/cortex.go:114` | `net_redex_new()` — `include/net_cortex.h:42` (post-split; previously `net.go.h:260`) |
| **RedEX disk** (persistent) | `new Redex({ persistentDir })` | `Redex(persistent_dir=...)` | `NewRedexWithPersistentDir(dir)` — `bindings/go/net/redex.go:337` | `net_redex_new_with_persistent_dir(path)` — `net_cortex.h` |
| **CortEX** (Tasks + Memories adapters, watchers) | `TasksAdapter`, `MemoriesAdapter` — `sdk-ts/src/cortex.ts:386,550` | `TasksAdapter`, `MemoriesAdapter`, `Task`, `Memory`, `*WatchIter` — `_net.pyi:340,426` | `OpenTasksAdapter` / `OpenMemoriesAdapter` — `bindings/go/net/tasks.go:235`, `memories.go:200`; plus `OpenTasks` / `OpenMemories` aliases in top-level `go/cortex.go` | `net_tasks_*`, `net_memories_*` — `include/net_cortex.h` (split out of `net.go.h`) |
| **NetDb** (cross-adapter façade + snapshot bundle) | `NetDb.open()` / `openFromSnapshot()` / `snapshot()` — `index.d.ts:1166`, sdk-ts `cortex.ts:708` | `net.NetDb` + `NetDbError` — `__init__.py:75-97` (runtime-only; stub does not declare it) | **Still not exposed.** No `bindings/go/net/netdb.go` despite the C FFI now shipping. P1.2 below | **Shipped post-survey.** `net_cortex.h:39,139-150` — `net_netdb_open / open_from_snapshot / snapshot / free_bundle / tasks / memories / close / free`. Borrow semantics on adapter accessors (lifetime bounded by parent NetDb, no double-free with `net_tasks_adapter_free`) |

### Notes

- **C header layout (post-split).** Four headers ship today: `net.h` (bus
  + shared error enum, 363 lines), `net_cortex.h` (RedEX / CortEX /
  NetDb, 156 lines — added in commit `33edd522`), `net_rpc.h` (RPC, 498
  lines), and the convenience header `net.go.h` (1528 lines, now
  `#include`s `net_cortex.h` and inlines RPC + Deck declarations). The
  three peer surfaces — `net_meshdb.h`, `net_meshos.h`, `net_deck.h` —
  each ship as separate cdylibs.
- **Python split.** Both tiers now ship: low-level PyO3 binding
  (`bindings/python` → `net._net`) AND `sdk-py/src/net_sdk/{redex,
  cortex, netdb, meshdb, meshos, deck}.py` ergonomic wrappers with
  re-exports + context managers. `from net_sdk.redex import Redex`
  and `from net import Redex` both work.
- **Feature gate.** The whole stack is behind the Rust `cortex` Cargo
  feature. Python's `__init__.py` try-imports it
  (`bindings/python/python/net/__init__.py:67-107`); wheels built without
  `cortex` silently omit the symbols.
- **NetDb gap on Go remains at the binding tier only.** The C FFI now
  ships (P1.1 done), and the top-level `go/netdb.go` already wraps it
  end-to-end (214 lines, full Tasks/Memories/Snapshot surface). But the
  parallel `bindings/go/net/netdb.go` is still missing — Go consumers
  importing the binding tier (`bindings/go/net`) instead of the
  top-level `go/` package have to open `Tasks` + `Memories` separately
  there. P1.2 below.

---

## 2. MeshDB / MeshOS

| Surface | Node | Python | Go | C |
|---|---|---|---|---|
| **MeshDB** (query layer: AST + runner + chain reader) | napi `bindings/node` — `MeshQuery`, `MeshQueryRunner`, `MeshQueryStream` (`index.d.ts:773,975,998`) + ergonomic `sdk-ts/src/meshdb.ts` wrapper with `parseMeshDbErrorKind` helper | PyO3 `net._net` (via `__init__.py:516-553`): `MeshQuery`, `MeshQueryRunner`, `InMemoryChainReader`, `QueryBuilder`, `Predicate`, `ResultRow`, `AggregateResult`, `JoinedRow`, `WindowBoundary`, `GroupKey`, `LineageEntry`, `CachePolicy`, `ExecuteOptions`, `MeshDbError` + `sdk-py/src/net_sdk/meshdb.py` wrapper with `runner_cm` context manager. Typed stub `_net.pyi` declares the classes at lines 1007-1023 but **without method signatures** | `bindings/go/net/meshdb.go` — `MeshDBReader`, `MeshDBResult`, `MeshDBResultRow`, `MeshDBError`, `NewMeshDBReader()`. Also re-exported / parallel-implemented at top-level `go/meshdb.go` (326 lines) | Dedicated `include/net_meshdb.h` + `libnet_meshdb` cdylib (9 prototypes — small surface: Reader / Runner / Query / Iter). Built from `bindings/go/meshdb-ffi` |
| **MeshOS** (daemon-author SDK) | napi `bindings/node` — `MeshOsDaemonHandle`, `MeshOsDaemonSdk` (`index.d.ts:773,833`) + `MeshOsConfigJs`. `registerDaemon(daemon: DaemonObjectTsfns, identity)` accepts full TSFN-bridged trait (process / snapshot / restore / onControl / health / saturation — see `bindings/node/src/meshos.rs:434-540`). Plus `sdk-ts/src/meshos.ts:263,338` ergonomic wrapper | PyO3 `net._net` — `MeshOsDaemonSdk`, `MeshOsDaemonHandle`, `MeshOsSdkError` (`__init__.py:356-401`). Plus high-level `sdk-py/src/net_sdk/meshos.py` — `MeshOsDaemon` Protocol, typed-dict envelopes (`DaemonControl`, `MaintenanceState`), context-manager wrappers. Typed stub `_net.pyi` declares the classes at 972-993 but **without method signatures** | `bindings/go/net/meshos.go` — full surface (~1.2k LOC): `MeshOsDaemonSdk`, `MeshOsDaemonHandle`, `MeshOsConfig`, `MeshOsDaemon` interface with Go-side callback trampolines (`Snapshot` / `Restore` / `OnControl` / `Health` / `Saturation`), `MeshOsMetadataView`, `PublishCapabilities`, context-aware variants. Also re-exported / parallel-implemented at top-level `go/meshos.go` (997 lines) | Dedicated `include/net_meshos.h` + `libnet_meshos` cdylib (~46 prototypes — full surface). Built from `bindings/go/meshos-ffi` |

### Notes

- **Both are separate cdylibs.** Unlike RedEX/CortEX (which the Go binding
  pulls in via `net.go.h` / `net_cortex.h`), MeshDB and MeshOS each ship
  their own header + shared library — `libnet_meshdb.{so,dylib,dll}` and
  `libnet_meshos.{so,dylib,dll}`. C consumers link the lib they want.
- **Wrapper parity now complete.** Both `sdk-ts` and `sdk-py` ship
  MeshDB + MeshOS wrappers (`sdk-ts/src/meshdb.ts`, `meshos.ts`,
  `sdk-py/src/net_sdk/meshdb.py`, `meshos.py`).
- **Python typed stub `_net.pyi` is partially stale.** Classes are
  declared past line 843 (`MeshOsDaemonSdk` at 972, `MeshDb` family at
  1007-1023, Deck family at 1117-1152) but **with empty bodies** — type
  checkers see the class exists but `.run()`, `.execute()`, `.admin()`
  etc. return `Any`. Compare to RedEX/CortEX (lines 249-544) which have
  full signatures.
- **MeshOS callback model is symmetric across bindings.** Go cgo
  trampolines (`goMeshOsProcessTrampoline`, …), Python Protocol class
  (`MeshOsDaemon`), Node TSFN-bridged daemon object
  (`DaemonObjectTsfns` in `bindings/node/src/meshos.rs:434-540`,
  exposed at the napi surface as the `daemon` argument to
  `registerDaemon`).
- **Cargo feature gates.** MeshDB is behind the `meshdb` feature; MeshOS
  is behind `meshos`. Wheels / dylibs built without them silently omit
  the symbols (Python's `__init__.py` try-imports both feature blocks).

---

## 3. Deck (operator control plane)

| Surface | Node | Python | Go | C |
|---|---|---|---|---|
| **Deck** (operator client + admin verbs + snapshot/status streams) | `DeckClient`, `OperatorIdentity`, `AdminCommands`, `DeckSnapshotStream`, `DeckStatusSummaryStream` — napi `bindings/node/index.d.ts:354,1433` + ergonomic wrapper `sdk-ts/src/deck.ts:70,309` (`DeckSdkError`, JSON-parsing helpers, async-iterable streams) | `DeckClient`, `OperatorIdentity`, `DeckAdminCommands`, `DeckSnapshotStream`, `DeckStatusSummaryStream` — PyO3 runtime + `sdk-py/src/net_sdk/deck.py` wrapper (JSON parsing, context managers, `.kind` on errors). Stub `_net.pyi:1115-1152` is **forward-declare only** — class names present, no method signatures | `bindings/go/net/deck.go` — `DeckClient`, `NewDeckClient`, `OperatorID`, admin verbs (`Drain` / `EnterMaintenance` / `Cordon` / `DropReplicas` / …), `DeckSnapshotStream`, `DeckStatusSummaryStream`. **Not re-exported from top-level `go/`** | `include/net_deck.h` + `libnet_deck` cdylib (924 lines — full surface incl. admin verbs, ICE break-glass factories, operator identity / registry / admin-verifier) |

### Notes

- **All four bindings expose Deck at the low-level binding tier.** Unlike
  NetDb (Go gap) or MeshDB (asymmetric wrapper coverage), Deck has full
  coverage in `bindings/node`, `bindings/python`, `bindings/go/net`, and
  `include/net_deck.h`. Both ergonomic wrappers (`sdk-ts/deck.ts`,
  `sdk-py/net_sdk/deck.py`) also exist.
- **Stub drift on Deck classes.** The Python `_net.pyi` stub declares 13
  Deck classes (`DeckClient`, `OperatorIdentity`, `DeckAdminCommands`,
  `DeckSnapshotStream`, `DeckStatusSummaryStream`, plus error/event
  types) at lines 1115-1152 but **without method bodies** — type
  checkers see the class exists but not its methods. Compare to
  RedEX/CortEX (lines 249-544) which have full method signatures. The
  runtime PyO3 binding has all methods.
- **Go top-level wrapper missing.** Unlike MeshDB / MeshOS (each
  shipped at both binding tier and top-level), Deck has no top-level
  `go/deck.go` companion. Deck callers must import the binding tier
  directly. P2.4 fixes this.

---

## Cross-cutting observations

1. **Two Python entry points, both populated.** `net._net` (PyO3, runtime
   ground truth) and `net_sdk` (pure-Python ergonomic wrappers) now both
   cover all seven surfaces (RedEX / CortEX / NetDb / MeshDB / MeshOS /
   Deck). Wrapper modules add re-exports + context managers; the runtime
   classes themselves live in `net._net`.
2. **Two Node entry points, both populated.** `@ai2070/net`
   (napi-rs `bindings/node`, ground truth) and `sdk-ts` (ergonomic
   wrappers) now both cover all seven surfaces. MeshDB is the most
   recent addition (`sdk-ts/src/meshdb.ts`).
3. **C ABI is six headers.** `net.h` (bus + error enum), `net_cortex.h`
   (RedEX + CortEX + NetDb — post-split), `net_rpc.h` (RPC),
   `net_meshdb.h` (MeshDB), `net_meshos.h` (MeshOS), `net_deck.h`
   (Deck). The convenience `net.go.h` `#include`s `net_cortex.h` and
   inlines RPC + Deck declarations for callers who want everything in
   one place.
4. **Remaining parity gaps** — much smaller than the original survey
   suggested:
   - `bindings/go/net/netdb.go` missing (top-level `go/netdb.go` already
     exists)
   - `go/deck.go` missing (top-level)
   - `_net.pyi` method bodies missing for MeshDB / MeshOS / Deck
     classes (~30 classes are forward-declared, not fully typed)
   - Cargo feature gates undocumented in binding READMEs

---

## Plan to fix

Grouped by impact. Items within a tier are roughly independent and can be
parallelized; cross-tier ordering matters (Tier 1 enables a couple of
Tier 2 items).

### Tier 1 — capability gaps (the binding can't do something a peer binding can)

**P1.1 — Land `NetDb` FFI surface.** ✅ **DONE** (commit `33edd522`).
Shipped in `include/net_cortex.h:39,139-150` with the seven prototypes
listed in the original plan, plus a `net_netdb_free_bundle` helper for
the snapshot bytes. Borrow semantics on `net_netdb_tasks` /
`net_netdb_memories` documented in the header at lines 130-138.

**P1.2 — Expose `NetDb` in the Go binding.** ✅ **DONE**.
`bindings/go/net/netdb.go` (215 lines) ships the `NetDb` type with
`OpenNetDb` / `OpenNetDbFromSnapshot` / `Tasks` / `Memories` /
`Snapshot` / `Close` / `Free`. Adapter accessors bridge per-file cgo
type walls via `newTasksAdapterFromRaw` / `newMemoriesAdapterFromRaw`
helpers added to `tasks.go` / `memories.go`. Test scaffold at
`bindings/go/net/netdb_test.go` covers open / accessor / snapshot
round-trip / adapter-survives-db-free / nil-redex scenarios.

**P1.3 — Node MeshOS daemon callback trait.** ✅ **DONE**.
`registerDaemon(daemon: DaemonObjectTsfns, identity: Identity)` at
`bindings/node/index.d.ts:753` accepts a full daemon object; the
`DaemonObjectTsfns` `FromNapiValue` impl at
`bindings/node/src/meshos.rs:434-540` builds TSFNs for `process` (required)
+ `snapshot` / `restore` / `onControl` / `health` / `saturation`
(optional) + reads `requiredCapabilities` / `optionalCapabilities` synchronously
at registration time.

### Tier 2 — DX / ergonomic parity (capability exists but is rough)

**P2.1 — `sdk-py` wrapper modules for RedEX / CortEX / NetDb / MeshDB.**
✅ **DONE**. Four modules shipped at
`sdk-py/src/net_sdk/{redex,cortex,netdb,meshdb}.py`, each with
re-exports + a context-manager helper (`open_file_cm`, `tasks_cm`,
`memories_cm`, `netdb_cm`, `runner_cm`).

**P2.2 — `sdk-ts` wrapper for MeshDB.** ✅ **DONE**.
`sdk-ts/src/meshdb.ts` (136 lines) re-exports the query AST + runner
classes from `@ai2070/net` and adds a `parseMeshDbErrorKind` helper
that unwraps the substrate's `<<meshdb-kind:KIND>>MESSAGE` envelope.

**P2.3 — Extend `_net.pyi` type stub method bodies.** ✅ **DONE**.
Stub grew from 1157 to 1779 lines (~622 added) with full method
signatures for the MeshOS region (`MeshOsDaemonSdk`,
`MeshOsDaemonHandle`), MeshDB region (`MeshDbError`,
`InMemoryChainReader`, `MeshQuery`, `MeshQueryRunner`, `QueryBuilder`,
plus newly-added `Predicate`, `ResultRow`, `AggregateResult`,
`JoinedRow`, `WindowBoundary`, `GroupKey`, `LineageEntry`,
`CachePolicy`, `ExecuteOptions`), and Deck region (`DeckClient`,
`OperatorIdentity`, `AdminCommands`, `SnapshotStream`,
`StatusSummaryStream`, `IceCommands`, `IceProposal`,
`SimulatedIceProposal`, `OperatorRegistry`, `AdminVerifier`).
Test scaffold at `bindings/python/tests/test_stub_drift.py`
parametrizes over every stub class and asserts the runtime symbol
matches.

**P2.4 — Top-level `go/` companion for Deck.** ✅ **DONE**.
`go/deck.go` (~480 lines) covers slice 1 of the Deck operator
surface: `DeckClient` lifecycle, all 9 `AdminCommands` verbs (Drain,
EnterMaintenance, ExitMaintenance, Cordon, Uncordon, DropReplicas,
InvalidatePlacement, RestartAllDaemons, ClearAvoidList), one-shot
Status / StatusSummary, snapshot + status-summary streams. Slice 2
(logs, failures, audit) and slice 3 (ICE break-glass) are deferred
to follow-up — those callers can use `bindings/go/net/deck.go`
directly. Test scaffold at `go/deck_test.go` covers lifecycle, seed
validation, status, admin verbs, and stream timeouts.

### Tier 3 — structural / documentation cleanup

**P3.1 — Split RedEX / CortEX out of `net.go.h` into a dedicated
`net_cortex.h`.** ✅ **DONE** (commit `33edd522`). `include/net_cortex.h`
now holds all `net_redex_*` / `net_tasks_*` / `net_memories_*` /
`net_netdb_*` prototypes; `net.go.h` `#include`s it at line 244.
Five-header layout in `include/README.md` reflects the current state.

**P3.2 — Cargo feature documentation.** ✅ **DONE**. "Cargo features"
section added to all 5 READMEs (`bindings/python/README.md`,
`bindings/node/README.md` — created, `bindings/go/net/README.md` —
created, `sdk-py/README.md`, `sdk-ts/README.md`). Each section
documents the `cortex` / `redex-disk` / `netdb` / `meshdb` / `meshos`
flags with their symbol coverage and the silent-omission behavior.

### Status

All survey items now shipped. The doc remains as a coverage map +
historical record of what was outstanding when first written; consult
the binding source for the authoritative current surface.

| Item | Status |
|---|---|
| P1.1 NetDb C FFI | ✅ Done (commit `33edd522`) |
| P1.2 `bindings/go/net/netdb.go` | ✅ Done |
| P1.3 Node MeshOS callback trait | ✅ Done |
| P2.1 sdk-py wrappers | ✅ Done |
| P2.2 sdk-ts MeshDB | ✅ Done |
| P2.3 `_net.pyi` method bodies | ✅ Done |
| P2.4 top-level `go/` companions | ✅ Done (Deck slice 1; slices 2/3 deferred) |
| P3.1 Header split (`net_cortex.h`) | ✅ Done (commit `33edd522`) |
| P3.2 Cargo feature docs | ✅ Done |

### Follow-up surfaces (out of original survey scope)

- Top-level `go/deck.go` covers slice 1 only. Slices 2 (log /
  failure / audit streams) and 3 (ICE break-glass) remain available
  via the binding tier (`bindings/go/net/deck.go`).
- The Python `_net.pyi` stub now matches the runtime surface for
  every feature-gated class. Add a CI check that runs
  `tests/test_stub_drift.py` against a wheel built with all features
  enabled to catch future drift.
